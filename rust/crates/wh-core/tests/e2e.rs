//! End to end: the real daemon (`run_daemon`) against a mock Wheelhouse that sends signed offers, a local bare git remote, and a fake
//! `claude` that writes files and prints recorded stream-json. Nothing here touches the network, the Keychain or the real `claude`.
//!
//! Covers accept, run, push, the checks fix loop, cancel, supersede, the rate-limit pause, a chat answer, restart recovery and the
//! replay of a finish that could not be sent while Wheelhouse was unreachable.
#![cfg(unix)]

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use wh_core::cancel::Cancel;
use wh_core::config::{update_config, Paths};
use wh_core::connection::iso_from_ms;
use wh_core::daemon::{run_daemon, DaemonOptions, Exit, Log};
use wh_core::state::JobRecords;
use wh_proto::keys;
use wh_proto::sign::sign_value;

const DEVICE: &str = "11111111-1111-1111-1111-111111111111";
const REPO: &str = "22222222-2222-2222-2222-222222222222";

// --- the mock Wheelhouse --------------------------------------------------------------------------------------------------------

struct Mock {
    url: String,
    key: ed25519_dalek::SigningKey,
    posts: Arc<Mutex<Vec<(String, Value)>>>,
    tx: broadcast::Sender<String>,
    streams: Arc<AtomicU64>,
    next_id: AtomicU64,
    /// While set, every finish is answered 503, as if Wheelhouse were down.
    fail_finish: Arc<AtomicBool>,
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, String, Value)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let len = head
        .lines()
        .find_map(|l| {
            l.to_lowercase()
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let mut first = head.lines().next()?.split_whitespace();
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let body = serde_json::from_slice(&buf[head_end..(head_end + len).min(buf.len())])
        .unwrap_or(Value::Null);
    Some((method, path, body))
}

fn json_response(status: u16, body: &str) -> String {
    format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
}

impl Mock {
    async fn start() -> Arc<Mock> {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (tx, _) = broadcast::channel(64);
        let mock = Arc::new(Mock {
            url: format!("http://{}", l.local_addr().unwrap()),
            key: keys::generate(),
            posts: Arc::default(),
            tx,
            streams: Arc::default(),
            next_id: AtomicU64::new(1),
            fail_finish: Arc::default(),
        });
        let m = mock.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = l.accept().await.unwrap();
                let m = m.clone();
                tokio::spawn(async move {
                    let Some((method, path, body)) = read_request(&mut sock).await else {
                        return;
                    };
                    if path == "/api/daemon/token" {
                        sock.write_all(
                            json_response(200, r#"{"accessToken":"whd_x","expiresIn":900}"#)
                                .as_bytes(),
                        )
                        .await
                        .ok();
                    } else if method == "GET" && path == "/api/daemon/stream" {
                        let mut rx = m.tx.subscribe();
                        sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n: hello\n\n").await.ok();
                        m.streams.fetch_add(1, Ordering::SeqCst);
                        while let Ok(frame) = rx.recv().await {
                            if sock.write_all(frame.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                    } else if path == "/api/daemon/hello" {
                        m.posts.lock().unwrap().push((path, body));
                        sock.write_all(
                            json_response(200, r#"{"device":{"poolSize":2,"chatModel":null}}"#)
                                .as_bytes(),
                        )
                        .await
                        .ok();
                    } else if path.ends_with("/finish") && m.fail_finish.load(Ordering::SeqCst) {
                        sock.write_all(
                            json_response(503, r#"{"error":{"message":"down"}}"#).as_bytes(),
                        )
                        .await
                        .ok();
                    } else {
                        m.posts.lock().unwrap().push((path, body));
                        sock.write_all(json_response(200, "{}").as_bytes())
                            .await
                            .ok();
                    }
                });
            }
        });
        mock
    }

    /// Send a message down the stream, signed by the server key.
    fn send(&self, message: &Value) {
        let data = json!({ "message": message, "signature": sign_value(&self.key, message) });
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(format!(
            "id: {id}\nevent: {}\ndata: {data}\n\n",
            message["type"].as_str().unwrap()
        ));
    }

    fn posts(&self) -> Vec<(String, Value)> {
        self.posts.lock().unwrap().clone()
    }

    fn post(&self, suffix: &str) -> Option<Value> {
        self.posts()
            .into_iter()
            .find(|p| p.0.ends_with(suffix))
            .map(|p| p.1)
    }

    /// Events of one run, in the order they went up.
    fn events(&self, id_path: &str) -> Vec<Value> {
        self.posts()
            .into_iter()
            .filter(|p| p.0 == format!("{id_path}/events"))
            .flat_map(|p| p.1["events"].as_array().cloned().unwrap_or_default())
            .collect()
    }
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..600 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

// --- a git remote and a fake claude ---------------------------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare repository with one commit on main.
fn remote(root: &Path) -> PathBuf {
    let (bare, seed) = (root.join("remote.git"), root.join("seed"));
    std::fs::create_dir_all(&seed).unwrap();
    git(root, &["init", "--bare", bare.to_str().unwrap()]);
    git(&seed, &["init"]);
    std::fs::write(seed.join("README.md"), "hello\n").unwrap();
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-m", "first"]);
    git(&seed, &["branch", "-M", "main"]);
    git(&seed, &["push", bare.to_str().unwrap(), "main"]);
    bare
}

/// The fake claude. What it does is chosen by the `mode` file in `dir`; `count` counts its runs.
fn fake_claude(dir: &Path) -> String {
    let f = dir.join("claude");
    let d = dir.display();
    std::fs::write(
        &f,
        format!(
            r#"#!/bin/sh
# What the daemon asks at every connect: not a run of the agent.
case "$1" in
  --version) echo "9.9.9 (Claude Code)"; exit 0 ;;
  auth) echo '{{"loggedIn":true,"subscriptionType":"max"}}'; exit 0 ;;
esac
out() {{ printf '%s\n' "$1"; }}
MODE=$(cat "{d}/mode" 2>/dev/null || echo write)
N=$(cat "{d}/count" 2>/dev/null || echo 0); N=$((N+1)); echo $N > "{d}/count"
printf '%s\0' "$@" > "{d}/args.$N"
case "$*" in
  *--include-partial-messages*)
    out '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"It is in "}}}}}}'
    out '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Grep","input":{{"pattern":"auth"}}}}]}}}}'
    out '{{"type":"result","is_error":false,"session_id":"sess-1","total_cost_usd":0.02,"result":"It is in auth/index.ts"}}'
    exit 0 ;;
esac
out '{{"type":"system","subtype":"init","model":"fake","session_id":"s1"}}'
case "$MODE" in
  hang)
    out '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"a.txt"}}}}]}}}}'
    sleep 300 &
    wait ;;
  ratelimit)
    out '{{"type":"result","is_error":true,"result":"Claude usage limit reached"}}'
    exit 1 ;;
  fix)
    echo "run $N" > agent.txt
    if [ "$N" -ge 2 ]; then touch fixed; fi
    out '{{"type":"result","is_error":false,"total_cost_usd":0.1,"result":"done"}}' ;;
  *)
    out '{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Writing the file"}}]}}}}'
    echo "written by the agent" > agent.txt
    out '{{"type":"result","is_error":false,"total_cost_usd":0.1,"result":"done\nLEARNING: tests live next to the source"}}' ;;
esac
"#
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    f.display().to_string()
}

// --- the harness ----------------------------------------------------------------------------------------------------------------

struct Harness {
    mock: Arc<Mock>,
    paths: Paths,
    bare: PathBuf,
    fake_dir: PathBuf,
    stop: Cancel,
    daemon: Option<tokio::task::JoinHandle<Result<Exit, String>>>,
    log: Arc<Mutex<Vec<String>>>,
    n: u32,
    _tmp: tempfile::TempDir,
}

impl Harness {
    /// A paired machine, a remote with one commit, and a daemon running against the mock.
    async fn start() -> Harness {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Mock::start().await;
        let paths = Paths::new(tmp.path().join("daemon"));
        let fake_dir = tmp.path().join("fake");
        std::fs::create_dir_all(&fake_dir).unwrap();
        let claude = fake_claude(&fake_dir);
        let bare = remote(tmp.path());
        let key = keys::generate();
        wh_core::keystore::store_key("file", &paths.home, &keys::export_private(&key)).unwrap();
        let mut patch = Map::new();
        for (k, v) in [
            ("server", json!(mock.url)),
            ("deviceId", json!(DEVICE)),
            (
                "serverPublicKey",
                json!(keys::raw_public(&mock.key.verifying_key())),
            ),
            ("keystore", json!("file")),
            ("claudePath", json!(claude)),
            ("poolSize", json!(2)),
        ] {
            patch.insert(k.into(), v);
        }
        update_config(patch, &paths).unwrap();
        let mut h = Harness {
            mock,
            paths,
            bare,
            fake_dir,
            stop: Cancel::new(),
            daemon: None,
            log: Arc::default(),
            n: 0,
            _tmp: tmp,
        };
        h.boot().await;
        h
    }

    /// Start (or restart) the daemon and wait until its stream is open.
    async fn boot(&mut self) {
        self.stop = Cancel::new();
        let log = self.log.clone();
        let log: Log = Arc::new(move |m| log.lock().unwrap().push(m));
        let mut o = DaemonOptions::new(self.paths.clone(), self.stop.clone(), log);
        o.adopt_path = false;
        o.heartbeat = Duration::from_millis(200);
        o.finish_retry = Some(Duration::from_millis(5));
        let before = self.mock.streams.load(Ordering::SeqCst);
        self.daemon = Some(tokio::spawn(run_daemon(o)));
        let mock = self.mock.clone();
        wait_for("the daemon's stream", move || {
            mock.streams.load(Ordering::SeqCst) > before
        })
        .await;
    }

    /// Stop the daemon the way a signal does: running work is cancelled and reported first.
    async fn shutdown(&mut self) -> Exit {
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(20), self.daemon.take().unwrap())
            .await
            .expect("the daemon stops")
            .unwrap()
            .unwrap()
    }

    fn mode(&self, m: &str) {
        std::fs::write(self.fake_dir.join("mode"), m).unwrap();
    }

    fn runs(&self) -> u32 {
        std::fs::read_to_string(self.fake_dir.join("count"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn offer(&mut self, extra: Value) -> Value {
        self.n += 1;
        let n = self.n;
        let mut o = json!({
            "type": "job.offer", "deviceId": DEVICE, "issuedAt": iso_from_ms(now()), "expiresAt": iso_from_ms(now() + 60_000),
            "jobId": format!("00000000-0000-0000-0000-{n:012}"), "model": "sonnet", "branch": format!("wh/WH-{n}-x"), "boardAccess": "none",
            "issue": { "id": "i", "ref": format!("WH-{n}"), "title": "Add agent.txt", "body": "Please." },
            "repository": { "id": REPO, "name": "app", "fullName": "acme/app", "cloneUrl": self.bare.display().to_string(), "defaultBranch": "main", "pathLimit": null },
        });
        o.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        o
    }

    fn chat(&mut self) -> Value {
        self.n += 1;
        json!({
            "type": "chat.message", "deviceId": DEVICE, "issuedAt": iso_from_ms(now()), "expiresAt": iso_from_ms(now() + 60_000),
            "messageId": format!("99999999-0000-0000-0000-{:012}", self.n), "threadId": "88888888-0000-0000-0000-000000000001",
            "model": "sonnet", "sessionId": null, "prompt": "Where is auth handled?",
            "repository": { "id": REPO, "name": "app", "fullName": "acme/app", "cloneUrl": self.bare.display().to_string(), "defaultBranch": "main" },
        })
    }

    fn control(&self, kind: &str, extra: Value) -> Value {
        let mut m = json!({ "type": kind, "deviceId": DEVICE, "issuedAt": iso_from_ms(now()) });
        m.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        m
    }

    fn job_path(m: &Value) -> String {
        format!("/api/daemon/jobs/{}", m["jobId"].as_str().unwrap())
    }

    async fn finished(&self, m: &Value) -> Value {
        let path = format!("{}/finish", Self::job_path(m));
        let mock = self.mock.clone();
        let p = path.clone();
        wait_for("the job's finish", move || {
            mock.posts().iter().any(|x| x.0 == p)
        })
        .await;
        self.mock
            .posts()
            .into_iter()
            .find(|x| x.0 == path)
            .unwrap()
            .1
    }

    /// The file on a branch of the remote, or None when the branch is not there.
    fn on_remote(&self, branch: &str, file: &str) -> Option<String> {
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(&self.bare)
            .args(["show", &format!("{branch}:{file}")])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

// --- scenarios ------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_job_is_accepted_run_checked_in_pushed_and_reported_and_its_worktree_goes() {
    let mut h = Harness::start().await;
    let m = h.offer(json!({}));
    h.mock.send(&m);
    let finish = h.finished(&m).await;

    let posts: Vec<String> = h
        .mock
        .posts()
        .into_iter()
        .map(|p| p.0)
        .filter(|p| !p.ends_with("hello") && !p.ends_with("heartbeat"))
        .collect();
    assert_eq!(
        posts[0],
        format!("{}/accept", Harness::job_path(&m)),
        "the accept goes first"
    );
    assert_eq!(finish["state"], "succeeded", "{finish}");
    assert_eq!(finish["pushed"], true);
    assert_eq!(finish["diffStats"]["files"], 1);
    assert_eq!(
        finish["learnings"],
        json!(["tests live next to the source"])
    );
    assert_eq!(finish["costUsd"], 0.1);
    let branch = m["branch"].as_str().unwrap();
    assert_eq!(
        h.on_remote(branch, "agent.txt").as_deref(),
        Some("written by the agent"),
        "the agent's work is on the remote branch"
    );
    assert_eq!(
        h.on_remote("main", "agent.txt"),
        None,
        "and main is untouched"
    );

    let events = h.mock.events(&Harness::job_path(&m));
    let phases: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "phase")
        .map(|e| e["data"]["phase"].as_str().unwrap())
        .collect();
    assert_eq!(phases, ["cloning", "worktree", "agent", "pushing"]);
    assert!(
        events.iter().any(|e| e["type"] == "started")
            && events
                .iter()
                .any(|e| e["type"] == "tool" || e["type"] == "text")
    );
    let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(
        seqs,
        (0..seqs.len() as u64).collect::<Vec<_>>(),
        "events are numbered in order"
    );

    let wt = h
        .paths
        .worktrees
        .join(REPO)
        .join(m["jobId"].as_str().unwrap());
    wait_for("the worktree to be removed", || !wt.exists()).await;
    assert_eq!(
        JobRecords::new(h.paths.jobs.clone()).all().len(),
        0,
        "no job record is left"
    );
    assert!(!h.shutdown().await.unpaired);
}

#[tokio::test]
async fn a_failed_check_goes_back_to_the_agent_and_the_fix_is_what_gets_pushed() {
    let mut h = Harness::start().await;
    h.mode("fix");
    let mut m = h.offer(json!({}));
    m["repository"]["checks"] =
        json!([{ "name": "fixed", "command": "test -f fixed", "required": true }]);
    h.mock.send(&m);
    let finish = h.finished(&m).await;
    assert_eq!(finish["state"], "succeeded", "{finish}");
    assert_eq!(h.runs(), 2, "the first run and one fix");
    assert_eq!(finish["checks"][0]["status"], "passed");
    assert_eq!(
        finish["checkedSha"].as_str().unwrap().len(),
        40,
        "the commit the checks ran on"
    );
    let branch = m["branch"].as_str().unwrap();
    assert_eq!(h.on_remote(branch, "agent.txt").as_deref(), Some("run 2"));
    // The agent was told which check failed.
    let second = std::fs::read(h.fake_dir.join("args.2")).unwrap();
    assert!(
        String::from_utf8_lossy(&second).contains("fixed"),
        "the fix prompt names the failed check"
    );
    let events = h.mock.events(&Harness::job_path(&m));
    assert!(events
        .iter()
        .any(|e| e["type"] == "phase" && e["data"]["phase"] == "fixing"));
    assert!(events.iter().filter(|e| e["type"] == "check").count() >= 2);
    h.shutdown().await;
}

#[tokio::test]
async fn a_check_that_never_passes_fails_the_job_after_two_fixes_but_the_work_is_still_pushed() {
    let mut h = Harness::start().await;
    let mut m = h.offer(json!({}));
    m["repository"]["checks"] =
        json!([{ "name": "never", "command": "echo nope; exit 1", "required": true }]);
    h.mock.send(&m);
    let finish = h.finished(&m).await;
    assert_eq!(finish["state"], "failed");
    assert!(
        finish["error"]
            .as_str()
            .unwrap()
            .contains("Check \"never\" failed: nope"),
        "{finish}"
    );
    assert_eq!(h.runs(), 3, "the first run and two fixes");
    assert_eq!(
        finish["pushed"], true,
        "the work is kept on the branch for a person to look at"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a_cancel_stops_the_agent_and_its_child_and_the_job_is_reported_canceled() {
    let mut h = Harness::start().await;
    h.mode("hang");
    let m = h.offer(json!({}));
    h.mock.send(&m);
    let events_path = Harness::job_path(&m);
    let mock = h.mock.clone();
    wait_for("the agent to start", move || {
        mock.events(&events_path)
            .iter()
            .any(|e| e["type"] == "tool")
    })
    .await;
    h.mock
        .send(&h.control("job.cancel", json!({ "jobId": m["jobId"] })));
    let finish = h.finished(&m).await;
    assert_eq!(finish["state"], "canceled");
    assert_eq!(finish["error"], "Stopped on request");
    assert_eq!(finish["pushed"], false, "the agent changed nothing");
    h.shutdown().await;
}

#[tokio::test]
async fn a_superseded_run_pushes_nothing_and_keeps_its_worktree() {
    let mut h = Harness::start().await;
    h.mode("hang");
    let m = h.offer(json!({}));
    h.mock.send(&m);
    let events_path = Harness::job_path(&m);
    let mock = h.mock.clone();
    wait_for("the agent to start", move || {
        mock.events(&events_path)
            .iter()
            .any(|e| e["type"] == "tool")
    })
    .await;
    h.mock.send(&h.control(
        "job.cancel",
        json!({ "jobId": m["jobId"], "superseded": true }),
    ));
    let finish = h.finished(&m).await;
    assert_eq!(finish["state"], "canceled");
    assert_eq!(finish["pushed"], false);
    assert!(finish["error"]
        .as_str()
        .unwrap()
        .contains("a newer run took over this branch"));
    assert_eq!(
        h.on_remote(m["branch"].as_str().unwrap(), "README.md"),
        None,
        "no branch was pushed"
    );
    assert!(
        h.paths
            .worktrees
            .join(REPO)
            .join(m["jobId"].as_str().unwrap())
            .exists(),
        "the worktree is kept"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a_run_that_hits_the_usage_limit_pauses_new_offers_and_chats() {
    let mut h = Harness::start().await;
    h.mode("ratelimit");
    let first = h.offer(json!({}));
    h.mock.send(&first);
    let finish = h.finished(&first).await;
    assert_eq!(finish["state"], "failed");

    let next = h.offer(json!({}));
    h.mock.send(&next);
    let path = format!("{}/reject", Harness::job_path(&next));
    let mock = h.mock.clone();
    let p = path.clone();
    wait_for("the refusal", move || mock.posts().iter().any(|x| x.0 == p)).await;
    let reject = h.mock.post(&path).unwrap();
    assert_eq!(reject["reason"], "not_ready");
    assert!(reject["detail"].as_str().unwrap().contains("usage limit"));
    assert_eq!(h.runs(), 1, "the second job never reached the agent");

    let chat = h.chat();
    h.mock.send(&chat);
    let cpath = format!(
        "/api/daemon/chat/{}/finish",
        chat["messageId"].as_str().unwrap()
    );
    let mock = h.mock.clone();
    let cp = cpath.clone();
    wait_for("the chat refusal", move || {
        mock.posts().iter().any(|x| x.0 == cp)
    })
    .await;
    let c = h.mock.post(&cpath).unwrap();
    assert_eq!(c["state"], "failed");
    assert!(c["error"].as_str().unwrap().contains("paused"));
    h.shutdown().await;
}

#[tokio::test]
async fn a_chat_message_is_answered_in_its_thread_worktree_with_deltas_and_the_session() {
    let mut h = Harness::start().await;
    let m = h.chat();
    h.mock.send(&m);
    let id = m["messageId"].as_str().unwrap().to_string();
    let path = format!("/api/daemon/chat/{id}");
    let mock = h.mock.clone();
    let fp = format!("{path}/finish");
    wait_for("the chat finish", move || {
        mock.posts().iter().any(|x| x.0 == fp)
    })
    .await;
    let finish = h.mock.post(&format!("{path}/finish")).unwrap();
    assert_eq!(
        finish,
        json!({ "state": "done", "sessionId": "sess-1", "costUsd": 0.02, "text": "It is in auth/index.ts" })
    );
    let kinds: Vec<String> = h
        .mock
        .events(&path)
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["delta", "tool"], "only deltas and tool calls go up");
    assert!(
        h.paths
            .chats
            .join(REPO)
            .join("88888888-0000-0000-0000-000000000001")
            .exists(),
        "the thread has a worktree"
    );
    let args =
        String::from_utf8_lossy(&std::fs::read(h.fake_dir.join("args.1")).unwrap()).into_owned();
    assert!(
        args.starts_with("-p\0Where is auth handled?\0") && args.contains("--restricted"),
        "the prompt as typed, read-only"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a_job_that_was_running_when_the_daemon_stopped_is_reported_failed_after_the_restart() {
    let mut h = Harness::start().await;
    h.shutdown().await;
    // What a crashed daemon leaves: a record for a job that never finished (no agent is running any more).
    JobRecords::new(h.paths.jobs.clone())
        .write(&json!({ "id": "lost-job", "repositoryId": REPO, "branch": "wh/x", "pid": null }))
        .unwrap();
    h.boot().await;
    let mock = h.mock.clone();
    wait_for("the failed finish", move || {
        mock.posts()
            .iter()
            .any(|x| x.0 == "/api/daemon/jobs/lost-job/finish")
    })
    .await;
    let finish = h.mock.post("/jobs/lost-job/finish").unwrap();
    assert_eq!(
        finish,
        json!({ "state": "failed", "error": "The daemon restarted while the job ran" })
    );
    assert!(JobRecords::new(h.paths.jobs.clone())
        .read("lost-job")
        .is_none());
    h.shutdown().await;
}

#[tokio::test]
async fn a_finish_that_could_not_be_sent_is_kept_and_sent_when_wheelhouse_is_back_even_after_a_restart(
) {
    let mut h = Harness::start().await;
    h.mock.fail_finish.store(true, Ordering::SeqCst);
    let m = h.offer(json!({}));
    h.mock.send(&m);
    let id = m["jobId"].as_str().unwrap().to_string();
    // The job runs and pushes; its finish is refused with a 503, so it is stored with the job's record.
    let jobs = h.paths.jobs.clone();
    let id2 = id.clone();
    wait_for("the finish to be stored", move || {
        JobRecords::new(jobs.clone())
            .read(&id2)
            .is_some_and(|r| r["pendingFinish"].is_object())
    })
    .await;
    assert!(
        h.mock
            .post(&format!("{}/finish", Harness::job_path(&m)))
            .is_none(),
        "nothing got through"
    );
    assert_eq!(
        h.on_remote(m["branch"].as_str().unwrap(), "agent.txt")
            .as_deref(),
        Some("written by the agent"),
        "the push did not wait for the report"
    );

    // The daemon goes away with the finish still pending, and comes back when Wheelhouse is reachable again.
    h.shutdown().await;
    h.mock.fail_finish.store(false, Ordering::SeqCst);
    h.boot().await;
    let finish = h.finished(&m).await;
    assert_eq!(finish["state"], "succeeded");
    assert_eq!(finish["pushed"], true);
    assert!(
        JobRecords::new(h.paths.jobs.clone()).read(&id).is_none(),
        "the record goes once the finish is received"
    );
    // The hello of the restarted daemon listed the job as still its own until the finish went out.
    let hellos: Vec<Value> = h
        .mock
        .posts()
        .into_iter()
        .filter(|p| p.0.ends_with("/hello"))
        .map(|p| p.1)
        .collect();
    assert!(hellos.len() >= 2);
    assert!(
        hellos.last().unwrap()["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["id"] == id.as_str()),
        "{:?}",
        hellos.last()
    );
    h.shutdown().await;
}

#[tokio::test]
async fn offers_that_are_not_trustworthy_are_ignored_and_a_stop_while_running_reports_the_daemon_going_down(
) {
    let mut h = Harness::start().await;
    // Signed by someone else, for another device, and expired: none gets an answer.
    let stranger = keys::generate();
    let forged = h.offer(json!({}));
    let data = json!({ "message": forged, "signature": sign_value(&stranger, &forged) });
    let _ = h
        .mock
        .tx
        .send(format!("id: 50\nevent: job.offer\ndata: {data}\n\n"));
    let other = h.offer(json!({ "deviceId": "someone-else" }));
    h.mock.send(&other);
    let expired = h.offer(json!({ "expiresAt": iso_from_ms(now() - 1000) }));
    h.mock.send(&expired);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        h.mock.posts().iter().all(|p| !p.0.contains("/jobs/")),
        "{:?}",
        h.mock.posts()
    );
    assert_eq!(h.runs(), 0);

    h.mode("hang");
    let m = h.offer(json!({}));
    h.mock.send(&m);
    let ep = Harness::job_path(&m);
    let mock = h.mock.clone();
    wait_for("the agent to start", move || {
        mock.events(&ep).iter().any(|e| e["type"] == "tool")
    })
    .await;
    assert!(!h.shutdown().await.unpaired);
    let finish = h
        .mock
        .post(&format!("{}/finish", Harness::job_path(&m)))
        .expect("reported before the daemon exits");
    assert_eq!(finish["state"], "canceled");
    assert!(finish["error"]
        .as_str()
        .unwrap()
        .contains("daemon was stopped"));
}
