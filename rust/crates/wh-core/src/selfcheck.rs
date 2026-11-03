//! The self-check: before a run's branch is pushed, the repository's own check commands (lint, typecheck, tests...) run in its worktree.
//! Each one ends as passed, failed (the command ran and exited non-zero) or could_not_run (it never got to run: no such tool, a bad
//! directory, a timeout, a stop). Nothing is skipped silently, so "lint was never run" cannot read as "lint passed".
//! Port of `daemon/src/selfcheck.mjs`.

use crate::cancel::Cancel;
use crate::runner::scrub;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command;

pub const MAX_FIX_ATTEMPTS: u32 = 2;
const TIMEOUT: Duration = Duration::from_secs(10 * 60);
const TAIL: usize = 3000;

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub name: String,
    pub command: String,
    pub cwd: Option<String>,
    pub required: bool,
}

impl Check {
    /// From the offer's `repository.checks` entry. `required` is true unless it is exactly false.
    pub fn from_json(v: &Value) -> Option<Check> {
        Some(Check {
            name: v["name"].as_str()?.to_string(),
            command: v["command"].as_str()?.to_string(),
            cwd: v["cwd"].as_str().map(String::from),
            required: v["required"] != json!(false),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Passed,
    Failed,
    CouldNotRun,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::CouldNotRun => "could_not_run",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub name: String,
    pub command: String,
    pub required: bool,
    pub status: Status,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub tail: String,
}

impl CheckResult {
    /// As reported to Wheelhouse. The live `check` event leaves the output out; the finish keeps it.
    pub fn to_json(&self, with_tail: bool) -> Value {
        let mut v = json!({
            "name": self.name, "command": self.command, "required": self.required, "status": self.status.as_str(),
            "exitCode": self.exit_code, "durationMs": self.duration_ms,
        });
        if with_tail {
            v["tail"] = json!(self.tail);
        }
        v
    }
}

/// `dir` joined with `rel`, `.` and `..` resolved by name only (as `path.resolve` does).
fn resolve(dir: &Path, rel: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for c in dir.join(rel).components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn last_chars(s: &str, n: usize) -> &str {
    let cut = s.len().saturating_sub(n);
    let cut = (cut..=s.len())
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(s.len());
    &s[cut..]
}

/// How a shell command ended.
pub enum Ended {
    Exited { code: Option<i32>, output: String },
    TimedOut,
    Stopped,
    CouldNotStart(String),
}

/// `/bin/sh -c command` in `cwd`: stdout then stderr as the output. Stops when `cancel` is raised or `timeout` passes.
pub async fn run_shell(cwd: &Path, command: &str, cancel: &Cancel, timeout: Duration) -> Ended {
    if cancel.is_cancelled() {
        return Ended::Stopped;
    }
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", command])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = match cmd.spawn_output() {
        Ok(c) => c,
        Err(e) => return Ended::CouldNotStart(e.to_string()),
    };
    tokio::select! {
        r = tokio::time::timeout(timeout, child) => match r {
            Err(_) => Ended::TimedOut,
            Ok(Err(e)) => Ended::CouldNotStart(e.to_string()),
            Ok(Ok(o)) => Ended::Exited {
                code: o.status.code(),
                output: format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
            },
        },
        _ = cancel.cancelled() => Ended::Stopped,
    }
}

/// `Command::output()` as a future that can be dropped (kill on drop is set) while the select above waits on it.
trait SpawnOutput {
    fn spawn_output(
        &mut self,
    ) -> std::io::Result<
        std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<std::process::Output>> + Send>,
        >,
    >;
}

impl SpawnOutput for Command {
    fn spawn_output(
        &mut self,
    ) -> std::io::Result<
        std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<std::process::Output>> + Send>,
        >,
    > {
        self.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = self.spawn()?;
        Ok(Box::pin(child.wait_with_output()))
    }
}

/// One check in `dir`. Never fails: whatever goes wrong is a result.
pub async fn run_check(check: &Check, dir: &Path, cancel: &Cancel, home: &str) -> CheckResult {
    let started = Instant::now();
    let result = |status: Status, exit_code: Option<i32>, output: &str| CheckResult {
        name: check.name.clone(),
        command: check.command.clone(),
        required: check.required,
        status,
        exit_code,
        duration_ms: started.elapsed().as_millis() as u64,
        tail: scrub(last_chars(output.trim(), TAIL), home),
    };
    let rel = check.cwd.as_deref().unwrap_or(".");
    let cwd = resolve(dir, rel);
    let dir = resolve(dir, ".");
    if cwd != dir && !cwd.starts_with(&dir) {
        return result(
            Status::CouldNotRun,
            None,
            "The check's directory is outside the worktree",
        );
    }
    if !cwd.exists() {
        return result(
            Status::CouldNotRun,
            None,
            &format!("The check's directory {rel} does not exist"),
        );
    }
    match run_shell(&cwd, &check.command, cancel, TIMEOUT).await {
        Ended::Exited {
            code: Some(0),
            output,
        } => result(Status::Passed, Some(0), &output),
        // 127 and 126: the shell could not find or execute the command.
        Ended::Exited {
            code: Some(c),
            output,
        } if c != 126 && c != 127 => result(Status::Failed, Some(c), &output),
        Ended::Exited { code, output } => result(Status::CouldNotRun, code, &output),
        Ended::TimedOut => result(
            Status::CouldNotRun,
            None,
            "The check took longer than 10 minutes and was stopped",
        ),
        Ended::Stopped => result(Status::CouldNotRun, None, "The check was stopped"),
        Ended::CouldNotStart(m) => result(Status::CouldNotRun, None, &m),
    }
}

/// Every check in order, each reported to `on_result` as it ends.
pub async fn run_checks(
    checks: &[Check],
    dir: &Path,
    cancel: &Cancel,
    home: &str,
    mut on_result: impl FnMut(&CheckResult),
) -> Vec<CheckResult> {
    let mut out = vec![];
    for c in checks {
        let r = run_check(c, dir, cancel, home).await;
        on_result(&r);
        out.push(r);
    }
    out
}

/// The check that decides the run, if any: the first required one that did not pass.
pub fn blocking(results: &[CheckResult]) -> Option<&CheckResult> {
    results
        .iter()
        .find(|c| c.required && c.status != Status::Passed)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn check(name: &str, command: &str, required: bool) -> Check {
        Check {
            name: name.into(),
            command: command.into(),
            cwd: None,
            required,
        }
    }

    #[tokio::test]
    async fn passed_failed_and_could_not_run_are_told_apart() {
        let d = tempfile::tempdir().unwrap();
        let c = Cancel::new();
        let r = run_checks(
            &[
                check("ok", "echo fine", true),
                check("bad", "echo 1 error; exit 1", true),
                check("gone", "no-such-tool-wh", true),
            ],
            d.path(),
            &c,
            "/home/x",
            |_| {},
        )
        .await;
        let got: Vec<_> = r
            .iter()
            .map(|r| (r.name.as_str(), r.status, r.exit_code))
            .collect();
        assert_eq!(
            got,
            [
                ("ok", Status::Passed, Some(0)),
                ("bad", Status::Failed, Some(1)),
                ("gone", Status::CouldNotRun, Some(127))
            ]
        );
        assert!(r[1].tail.contains("1 error"));
        assert_eq!(blocking(&r).unwrap().name, "bad");
        assert!(blocking(&r[..1]).is_none());
        assert!(blocking(&[check_result_optional()]).is_none());
    }

    fn check_result_optional() -> CheckResult {
        CheckResult {
            name: "o".into(),
            command: "x".into(),
            required: false,
            status: Status::Failed,
            exit_code: Some(1),
            duration_ms: 0,
            tail: String::new(),
        }
    }

    #[tokio::test]
    async fn a_directory_outside_the_worktree_or_missing_could_not_run() {
        let d = tempfile::tempdir().unwrap();
        let c = Cancel::new();
        let mut out = check("a", "true", true);
        out.cwd = Some("../elsewhere".into());
        let r = run_check(&out, d.path(), &c, "/h").await;
        assert_eq!(r.status, Status::CouldNotRun);
        assert!(r.tail.contains("outside the worktree"));
        let mut missing = check("a", "true", true);
        missing.cwd = Some("nope".into());
        let r = run_check(&missing, d.path(), &c, "/h").await;
        assert!(r.tail.contains("does not exist"), "{}", r.tail);
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let mut sub = check("a", "pwd", true);
        sub.cwd = Some("sub".into());
        assert_eq!(
            run_check(&sub, d.path(), &c, "/h").await.status,
            Status::Passed
        );
    }

    #[tokio::test]
    async fn a_stop_ends_a_running_check_as_could_not_run() {
        let d = tempfile::tempdir().unwrap();
        let c = Cancel::new();
        let c2 = c.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c2.cancel();
        });
        let t = Instant::now();
        let r = run_check(&check("slow", "sleep 20", true), d.path(), &c, "/h").await;
        assert_eq!(r.status, Status::CouldNotRun);
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn checks_read_from_the_offer_and_required_defaults_to_true() {
        let c = Check::from_json(&json!({ "name": "lint", "command": "x" })).unwrap();
        assert!(c.required);
        assert!(
            !Check::from_json(&json!({ "name": "l", "command": "x", "required": false }))
                .unwrap()
                .required
        );
        assert!(Check::from_json(&json!({ "name": "l" })).is_none());
    }
}
