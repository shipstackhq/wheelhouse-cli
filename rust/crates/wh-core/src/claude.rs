//! Running the claude CLI: the process, its stream-json output, stopping it, and the private MCP config file.
//! Port of `runCli`, `withMcpConfig` and `runClaude` in `daemon/src/runner/claude.mjs`. Shared by jobs and chats.

use crate::cancel::Cancel;
use crate::config::{write_private, RunnerConfig};
use crate::runner::{claude_args, events_from, short, RunEvent};
use crate::supervisor::{EventSink, RunOutput, RunResult, Stopped};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

/// How long a stopped run gets to exit on SIGTERM before its process group is killed.
const KILL_AFTER: Duration = Duration::from_secs(10);

/// Turns one stdout line into progress events. Chat uses a stateful one.
pub type Parser = Box<dyn FnMut(&str) -> Vec<RunEvent> + Send>;

pub struct CliRun {
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub claude_path: String,
    pub cancel: Cancel,
    pub timeout: Duration,
    pub parse: Parser,
    pub on_event: EventSink,
    pub on_spawn: Arc<dyn Fn(u32) + Send + Sync>,
}

/// The default parser: stream-json as the job runner reads it.
pub fn job_parser(home: String) -> Parser {
    Box::new(move |line| events_from(line, &home))
}

fn output_from(data: &Value) -> RunOutput {
    RunOutput {
        is_error: data["isError"] == json!(true),
        text: data["text"].as_str().unwrap_or("").to_string(),
        session_id: data["sessionId"].as_str().map(String::from),
        cost_usd: data["costUsd"].as_f64(),
        learnings: data["learnings"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(unix)]
fn signal_group(pid: u32, sig: i32) {
    // SAFETY: kill(2) with a negative pid signals a process group; the worst a stale pid does is an ESRCH error, which is ignored.
    unsafe {
        libc::kill(-(pid as i32), sig);
    }
}

/// Run claude in `cwd` with `args`. Ends with a result when it ends; `cancel` stops it (SIGTERM to its process group, then SIGKILL after
/// ten seconds). `parse` turns each stdout line into progress events, `on_event` gets them, `on_spawn` the child's pid (for the job record).
pub async fn run_cli(mut run: CliRun) -> RunResult {
    let mut cmd = Command::new(&run.claude_path);
    cmd.args(&run.args)
        .current_dir(&run.cwd)
        .env("CLAUDE_CODE_ENTRYPOINT", "wheelhouse-daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let error = if e.kind() == std::io::ErrorKind::NotFound {
                format!("claude not found at \"{}\"", run.claude_path)
            } else {
                e.to_string()
            };
            return RunResult {
                ok: false,
                error: Some(error),
                ..Default::default()
            };
        }
    };
    let pid = child.id();
    if let Some(pid) = pid {
        (run.on_spawn)(pid);
    }
    let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
    let mut stderr_pipe = child.stderr.take().expect("piped");
    let stderr_task = tokio::spawn(async move {
        // Only the last 4000 characters are kept, as in Node.
        let mut kept = String::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr_pipe.read(&mut buf).await {
            if n == 0 {
                break;
            }
            kept.push_str(&String::from_utf8_lossy(&buf[..n]));
            if kept.len() > 4000 {
                let mut cut = kept.len() - 4000;
                while !kept.is_char_boundary(cut) {
                    cut += 1;
                }
                kept.drain(..cut);
            }
        }
        kept
    });

    let mut killed: Option<Stopped> = None;
    let mut result: Option<RunOutput> = None;
    let timeout = tokio::time::sleep(run.timeout);
    tokio::pin!(timeout);
    let mut line = Vec::new();
    loop {
        line.clear();
        tokio::select! {
            n = stdout.read_until(b'\n', &mut line) => {
                if !matches!(n, Ok(n) if n > 0) {
                    break;
                }
                let text = String::from_utf8_lossy(&line);
                for e in (run.parse)(text.trim_end_matches('\n')) {
                    if e.kind == "result" {
                        result = Some(output_from(&e.data));
                    }
                    (run.on_event)(e);
                }
            }
            _ = &mut timeout, if killed.is_none() => killed = stop(pid, Stopped::Timeout),
            _ = run.cancel.cancelled(), if killed.is_none() => killed = stop(pid, Stopped::Canceled),
        }
    }
    let status = child.wait().await.ok();
    let stderr = stderr_task.await.unwrap_or_default();
    let exit_code = status.and_then(|s| s.code());
    RunResult {
        ok: killed.is_none()
            && exit_code == Some(0)
            && result.as_ref().is_some_and(|r| !r.is_error),
        stopped: killed,
        exit_code,
        error: None,
        stderr: short(&stderr, 1000),
        result,
    }
}

/// Ask the process group to stop now and kill it if it is still there ten seconds later.
fn stop(pid: Option<u32>, why: Stopped) -> Option<Stopped> {
    #[cfg(unix)]
    if let Some(pid) = pid {
        signal_group(pid, libc::SIGTERM);
        tokio::spawn(async move {
            tokio::time::sleep(KILL_AFTER).await;
            signal_group(pid, libc::SIGKILL);
        });
    }
    #[cfg(not(unix))]
    let _ = pid;
    Some(why)
}

/// The Wheelhouse MCP config for `token`, written 0600 in its own temp directory (outside the worktree, so the agent cannot read it, and
/// not in argv, where `ps` would show the key). The file and its directory go when this is dropped.
pub struct McpConfig {
    dir: tempfile::TempDir,
}

impl McpConfig {
    pub fn new(token: &str, server: &str) -> std::io::Result<McpConfig> {
        let dir = tempfile::Builder::new().prefix("wh-mcp-").tempdir()?;
        let config = json!({ "mcpServers": { "wheelhouse": {
            "type": "http", "url": format!("{}/mcp", server.trim_end_matches('/')), "headers": { "Authorization": format!("Bearer {token}") },
        } } });
        write_private(&dir.path().join("mcp.json"), config.to_string().as_bytes())?;
        Ok(McpConfig { dir })
    }

    pub fn path(&self) -> PathBuf {
        self.dir.path().join("mcp.json")
    }
}

pub struct ClaudeRun<'a> {
    pub offer: Value,
    pub cwd: PathBuf,
    pub runner: &'a RunnerConfig,
    /// Wheelhouse's address, for the MCP config; without it (or a token) the run has no MCP.
    pub server: Option<&'a str>,
    pub claude_path: String,
    pub cancel: Cancel,
    pub home: String,
    pub on_event: EventSink,
    pub on_spawn: Arc<dyn Fn(u32) + Send + Sync>,
    /// Defaults to the runner's limit.
    pub timeout: Option<Duration>,
}

/// Run one job's agent in `cwd`.
pub async fn run_claude(r: ClaudeRun<'_>) -> RunResult {
    let token = r.offer["mcp"]["token"].as_str().filter(|t| !t.is_empty());
    let mcp = match (token, r.server.filter(|s| !s.is_empty())) {
        (Some(t), Some(s)) => match McpConfig::new(t, s) {
            Ok(m) => Some(m),
            Err(e) => {
                return RunResult {
                    ok: false,
                    error: Some(format!("could not write the MCP config: {e}")),
                    ..Default::default()
                }
            }
        },
        _ => None,
    };
    let path = mcp.as_ref().map(|m| m.path());
    let args = claude_args(&r.offer, r.runner, path.as_deref().and_then(Path::to_str));
    let out = run_cli(CliRun {
        args,
        cwd: r.cwd,
        claude_path: r.claude_path,
        cancel: r.cancel,
        timeout: r
            .timeout
            .unwrap_or(Duration::from_secs(r.runner.timeout_minutes * 60)),
        parse: job_parser(r.home),
        on_event: r.on_event,
        on_spawn: r.on_spawn,
    })
    .await;
    drop(mcp);
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::default_runner;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// A stand-in for the claude CLI: prints stream-json like `claude -p --output-format stream-json`, records its arguments (NUL
    /// separated, in `args.bin` in its working directory), and with a `hang` file there stays alive until it is killed, with a child of its own.
    fn fake_claude(dir: &Path) -> String {
        let f = dir.join("claude");
        std::fs::write(
            &f,
            r#"#!/bin/sh
printf '%s\0' "$@" > args.bin
echo '{"type":"system","subtype":"init","model":"claude-sonnet","session_id":"s1"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Looking at the code"},{"type":"tool_use","name":"Edit","input":{"file_path":"src/a.ts"}}]}}'
if [ -f hang ]; then
  sleep 300 &
  echo $! > child.pid
  wait
else
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.12,"duration_ms":900,"result":"Changed a.ts"}'
fi
"#,
        )
        .unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        f.display().to_string()
    }

    fn offer() -> Value {
        json!({ "jobId": "j", "model": "sonnet", "branch": "wh/WH-9-fix",
            "issue": { "ref": "WH-9", "title": "Fix it", "body": "Steps to reproduce" },
            "repository": { "name": "app", "fullName": "acme/app", "defaultBranch": "main", "pathLimit": null } })
    }

    struct Setup {
        _tmp: tempfile::TempDir,
        claude: String,
        cwd: PathBuf,
    }

    fn setup() -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let claude = fake_claude(tmp.path());
        let cwd = tmp.path().join("wt");
        std::fs::create_dir(&cwd).unwrap();
        Setup {
            _tmp: tmp,
            claude,
            cwd,
        }
    }

    fn args_of(cwd: &Path) -> Vec<String> {
        let raw = std::fs::read(cwd.join("args.bin")).unwrap();
        raw.split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    }

    fn collect() -> (EventSink, Arc<Mutex<Vec<RunEvent>>>) {
        let events = Arc::new(Mutex::new(vec![]));
        let e2 = events.clone();
        (Arc::new(move |e| e2.lock().unwrap().push(e)), events)
    }

    fn run<'a>(
        s: &Setup,
        offer: Value,
        runner: &'a RunnerConfig,
        on_event: EventSink,
        cancel: Cancel,
    ) -> ClaudeRun<'a> {
        ClaudeRun {
            offer,
            cwd: s.cwd.clone(),
            runner,
            server: None,
            claude_path: s.claude.clone(),
            cancel,
            home: "/home/x".into(),
            on_event,
            on_spawn: Arc::new(|_| {}),
            timeout: None,
        }
    }

    #[tokio::test]
    async fn runs_the_cli_in_the_worktree_and_reports_its_result() {
        let s = setup();
        let (sink, events) = collect();
        let runner = default_runner();
        let r = run_claude(run(&s, offer(), &runner, sink, Cancel::new())).await;
        assert!(r.ok, "{r:?}");
        assert_eq!(r.result.as_ref().unwrap().cost_usd, Some(0.12));
        let ev = events.lock().unwrap();
        assert_eq!(
            ev.iter().map(|e| e.kind).collect::<Vec<_>>(),
            ["started", "text", "tool", "result"]
        );
        assert_eq!(ev[2].data, json!({ "name": "Edit", "summary": "src/a.ts" }));
        assert_eq!(args_of(&s.cwd)[0], "-p");
    }

    #[tokio::test]
    async fn an_abort_stops_the_agent_and_its_children() {
        let s = setup();
        std::fs::write(s.cwd.join("hang"), "").unwrap();
        let cancel = Cancel::new();
        let c2 = cancel.clone();
        let sink: EventSink = Arc::new(move |e| {
            if e.kind == "tool" {
                c2.cancel();
            }
        });
        let runner = default_runner();
        let r = tokio::time::timeout(
            Duration::from_secs(20),
            run_claude(run(&s, offer(), &runner, sink, cancel)),
        )
        .await
        .expect("the run ended");
        assert!(!r.ok);
        assert_eq!(r.stopped, Some(Stopped::Canceled));
        let child: i32 = std::fs::read_to_string(s.cwd.join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        // SAFETY: signal 0 only checks that the process exists.
        let alive = unsafe { libc::kill(child, 0) } == 0;
        assert!(!alive, "the agent's child was stopped too");
    }

    #[tokio::test]
    async fn a_run_past_its_time_limit_is_stopped_as_a_timeout() {
        let s = setup();
        std::fs::write(s.cwd.join("hang"), "").unwrap();
        let runner = default_runner();
        let mut r = run(&s, offer(), &runner, collect().0, Cancel::new());
        r.timeout = Some(Duration::from_millis(300));
        let out = run_claude(r).await;
        assert_eq!((out.ok, out.stopped), (false, Some(Stopped::Timeout)));
    }

    #[tokio::test]
    async fn a_missing_cli_is_a_clear_failure() {
        let s = setup();
        let runner = default_runner();
        let mut r = run(&s, offer(), &runner, collect().0, Cancel::new());
        r.claude_path = s.cwd.join("nope").display().to_string();
        let out = run_claude(r).await;
        assert!(!out.ok);
        assert!(out.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn a_failing_exit_or_an_error_result_is_not_ok() {
        let s = setup();
        std::fs::write(
            s.cwd.join("../claude"),
            "#!/bin/sh\necho '{\"type\":\"result\",\"is_error\":true,\"result\":\"boom\"}'\n",
        )
        .unwrap();
        let runner = default_runner();
        let out = run_claude(run(&s, offer(), &runner, collect().0, Cancel::new())).await;
        assert!(!out.ok);
        assert_eq!(
            (out.exit_code, out.result.unwrap().text.as_str()),
            (Some(0), "boom")
        );
    }

    #[tokio::test]
    async fn a_run_with_a_key_reads_wheelhouse_through_a_private_mcp_config_file_that_is_gone_afterwards(
    ) {
        let s = setup();
        let runner = default_runner();
        assert!(
            !claude_args(&offer(), &runner, None).contains(&"--mcp-config".to_string()),
            "no key, no MCP"
        );
        let mut keyed = offer();
        keyed["issue"]["team"] = json!({ "key": "WH" });
        keyed["mcp"] = json!({ "token": "wh_secret" });
        let mut r = run(&s, keyed, &runner, collect().0, Cancel::new());
        r.server = Some("http://wh.test/");
        // Read the file while the run is alive: a fake that copies it aside.
        std::fs::write(
            s.cwd.join("../claude"),
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > args.bin\nwhile [ \"$1\" != \"--mcp-config\" ]; do shift; done\ncp \"$2\" seen.json\nstat -f %Lp \"$2\" > mode 2>/dev/null || stat -c %a \"$2\" > mode\necho '{\"type\":\"result\",\"is_error\":false,\"result\":\"ok\"}'\n",
        )
        .unwrap();
        assert!(run_claude(r).await.ok);
        let a = args_of(&s.cwd);
        let file = a[a.iter().position(|x| x == "--mcp-config").unwrap() + 1].clone();
        assert!(a[a.iter().position(|x| x == "--allowedTools").unwrap() + 1]
            .split(',')
            .any(|t| t == "mcp__wheelhouse"));
        assert!(
            !a.join(" ").contains("wh_secret"),
            "the key is never in argv"
        );
        assert!(
            !file.starts_with(s.cwd.to_str().unwrap()),
            "outside the worktree"
        );
        assert!(!Path::new(&file).exists(), "removed when the run ends");
        let seen: Value =
            serde_json::from_str(&std::fs::read_to_string(s.cwd.join("seen.json")).unwrap())
                .unwrap();
        assert_eq!(
            seen["mcpServers"]["wheelhouse"]["url"],
            "http://wh.test/mcp"
        );
        assert_eq!(
            seen["mcpServers"]["wheelhouse"]["headers"]["Authorization"],
            "Bearer wh_secret"
        );
        assert_eq!(
            std::fs::read_to_string(s.cwd.join("mode")).unwrap().trim(),
            "600"
        );
    }
}
