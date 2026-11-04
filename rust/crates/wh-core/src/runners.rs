//! The real agent side of the supervisor: `Runners` backed by the claude CLI. In Node, `daemon.mjs` wires `runClaude` and `runChat` into
//! the supervisor with the config's runner policy, the server address and the `claude` that was found on the user's PATH; this does the same.

use crate::chat::{run_chat, ChatRunArgs};
use crate::claude::{run_claude, ClaudeRun};
use crate::config::RunnerConfig;
use crate::supervisor::{ChatRun, JobRun, RunResult, Runners};

pub struct ClaudeRunners {
    pub runner: RunnerConfig,
    /// Wheelhouse's address, for the read-only MCP config a run's key opens.
    pub server: String,
    pub claude_path: String,
    /// The person's home folder, so what a run reports never names it.
    pub home: String,
}

impl Runners for ClaudeRunners {
    async fn run_job(&self, run: JobRun) -> RunResult {
        run_claude(ClaudeRun {
            offer: run.offer,
            cwd: run.cwd,
            runner: &self.runner,
            server: Some(&self.server),
            claude_path: self.claude_path.clone(),
            cancel: run.cancel,
            home: self.home.clone(),
            on_event: run.on_event,
            on_spawn: run.on_spawn,
            timeout: None,
        })
        .await
    }

    async fn run_chat(&self, run: ChatRun) -> RunResult {
        run_chat(ChatRunArgs {
            message: run.message,
            checkout: run.checkout,
            cwd: run.cwd,
            server: Some(self.server.clone()),
            claude_path: self.claude_path.clone(),
            cancel: run.cancel,
            home: self.home.clone(),
            on_event: run.on_event,
            // A chat has no job record, so nothing keeps its pid (as in Node).
            on_spawn: std::sync::Arc::new(|_| {}),
            timeout: None,
        })
        .await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::cancel::Cancel;
    use crate::config::default_runner;
    use crate::workspace::ChatWorktree;
    use serde_json::{json, Value};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn runners(dir: &std::path::Path, script: &str) -> ClaudeRunners {
        let f = dir.join("claude");
        std::fs::write(&f, script).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        ClaudeRunners {
            runner: default_runner(),
            server: "http://wh.test".into(),
            claude_path: f.display().to_string(),
            home: "/home/x".into(),
        }
    }

    const SCRIPT: &str = r#"#!/bin/sh
printf '%s\0' "$@" > args.bin
echo '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}}'
echo '{"type":"result","subtype":"success","is_error":false,"session_id":"s1","total_cost_usd":0.5,"result":"the answer"}'
"#;

    #[tokio::test]
    async fn a_job_run_uses_the_job_parser_and_reports_the_pid_and_a_chat_run_uses_the_chat_parser()
    {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("wt");
        std::fs::create_dir(&cwd).unwrap();
        let r = runners(tmp.path(), SCRIPT);

        let (events, pids) = (Arc::new(Mutex::new(vec![])), Arc::new(Mutex::new(vec![])));
        let (e2, p2) = (events.clone(), pids.clone());
        let job = r
            .run_job(JobRun {
                offer: json!({ "jobId": "j", "model": "sonnet", "branch": "wh/x", "issue": { "ref": "WH-1", "title": "t", "body": "" }, "repository": { "name": "app" } }),
                cwd: cwd.clone(),
                cancel: Cancel::new(),
                on_event: Arc::new(move |e| e2.lock().unwrap().push(e.kind)),
                on_spawn: Arc::new(move |p| p2.lock().unwrap().push(p)),
            })
            .await;
        assert!(job.ok, "{job:?}");
        assert_eq!(job.result.unwrap().cost_usd, Some(0.5));
        assert_eq!(
            *events.lock().unwrap(),
            ["result"],
            "the job parser ignores partial-message lines"
        );
        assert_eq!(pids.lock().unwrap().len(), 1);
        let args = std::fs::read(cwd.join("args.bin")).unwrap();
        assert!(String::from_utf8_lossy(&args).starts_with("-p\0"));

        let chat_events = Arc::new(Mutex::new(vec![]));
        let c2 = chat_events.clone();
        let chat = r
            .run_chat(ChatRun {
                message: json!({ "messageId": "m", "model": "opus", "prompt": "hello?", "sessionId": null, "repository": { "name": "app", "defaultBranch": "main" } }),
                checkout: ChatWorktree { dir: PathBuf::new(), commit: "abc".into(), branches: vec![] },
                cwd: cwd.clone(),
                cancel: Cancel::new(),
                on_event: Arc::new(move |e| c2.lock().unwrap().push((e.kind, e.data))),
            })
            .await;
        assert!(chat.ok, "{chat:?}");
        let got = chat_events.lock().unwrap().clone();
        assert_eq!(got[0], ("delta", json!({ "text": "hi" })));
        assert_eq!(got[1].0, "result");
        assert_eq!(got[1].1["sessionId"], Value::from("s1"));
        let args =
            String::from_utf8_lossy(&std::fs::read(cwd.join("args.bin")).unwrap()).into_owned();
        assert!(
            args.contains("--include-partial-messages") && args.starts_with("-p\0hello?\0"),
            "chat arguments, not job arguments"
        );
    }

    #[tokio::test]
    async fn a_missing_claude_fails_the_run_instead_of_the_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = runners(tmp.path(), SCRIPT);
        r.claude_path = tmp.path().join("nope").display().to_string();
        let out = r
            .run_job(JobRun {
                offer: json!({ "jobId": "j", "model": "sonnet", "issue": { "ref": "WH-1" }, "repository": { "name": "app" } }),
                cwd: tmp.path().to_path_buf(),
                cancel: Cancel::new(),
                on_event: Arc::new(|_| {}),
                on_spawn: Arc::new(|_| {}),
            })
            .await;
        assert!(!out.ok && out.error.unwrap().contains("not found"));
    }
}
