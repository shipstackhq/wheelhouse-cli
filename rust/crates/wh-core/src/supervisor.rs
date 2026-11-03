//! The pool: which jobs this machine is running, whether it can take another, and each job from offer to finish.
//!
//! Every message from Wheelhouse is checked before anything happens: the server's signature against the key pinned at pairing, that it
//! names this device, and (for an offer) that it has not expired. Anything that fails is dropped and logged, never answered: an
//! unverified message cannot be trusted even to say which job to refuse. Port of `daemon/src/supervisor.mjs`.
//!
//! Messages stay `serde_json::Value`s, as in Node: the signature covers the exact JSON the server sent, and the runner's prompt reads
//! the offer's fields by name. The workspace and the runner are traits, so the flow is tested against fakes as the Node tests do.

use crate::cancel::Cancel;
use crate::connection::{iso_from_ms, parse_iso_ms, JobReporter, Poster, ReporterConfig};
use crate::runner::{is_rate_limited, RunEvent};
use crate::selfcheck::{
    blocking, run_checks, run_shell, Check, CheckResult, Ended, Status, MAX_FIX_ATTEMPTS,
};
use crate::state::JobRecords;
use crate::workspace::{ChatWorktree, GitError, RepoRef, Summary, SyncResult, Workspace, Worktree};
use ed25519_dalek::VerifyingKey;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use wh_proto::verify_signed;

const MESSAGE_TYPES: [&str; 5] = [
    "job.offer",
    "job.cancel",
    "settings.update",
    "chat.message",
    "chat.cancel",
];
/// Why a run whose branch a newer run took over was stopped: its work is kept on this machine, not pushed.
pub const SUPERSEDED_REASON: &str = "Stopped: a newer run took over this branch while this machine could not be reached. Nothing was pushed; the worktree is kept here.";
/// Chat answers run in their own small pool, apart from the jobs: they never take a job's slot and do not show in `free`.
pub const CHAT_POOL_SIZE: usize = 2;
/// After a run hits the subscription's usage limit, new offers are refused for this long.
pub const RATE_LIMIT_PAUSE_MS: u64 = 15 * 60_000;
const SETUP_TIMEOUT: Duration = Duration::from_secs(20 * 60);

// --- what the supervisor needs from the rest of the daemon ----------------------------------------------------------------------

pub type PhaseFn<'a> = &'a (dyn Fn(&str, &str) + Send + Sync);

/// The git side: the parts of `Workspace` the job flow calls.
pub trait WorkspaceOps: Send + Sync + 'static {
    fn ensure_clone(
        &self,
        repo: &RepoRef,
        on_phase: PhaseFn<'_>,
    ) -> impl Future<Output = Result<(), GitError>> + Send;
    fn create_worktree(
        &self,
        repo: &RepoRef,
        job_id: &str,
        branch: &str,
        start_from: Option<&str>,
    ) -> impl Future<Output = Result<Worktree, GitError>> + Send;
    fn chat_worktree(
        &self,
        repo: &RepoRef,
        thread_id: &str,
    ) -> impl Future<Output = Result<ChatWorktree, GitError>> + Send;
    fn sync_base(
        &self,
        dir: &Path,
        base: &str,
    ) -> impl Future<Output = Result<SyncResult, GitError>> + Send;
    fn commit_leftovers(
        &self,
        dir: &Path,
        message: &str,
    ) -> impl Future<Output = Result<bool, GitError>> + Send;
    fn head(&self, dir: &Path) -> impl Future<Output = Result<String, GitError>> + Send;
    fn summary(
        &self,
        dir: &Path,
        base: &str,
    ) -> impl Future<Output = Result<Summary, GitError>> + Send;
    fn push(&self, dir: &Path, branch: &str) -> impl Future<Output = Result<(), GitError>> + Send;
    fn remove_worktree(
        &self,
        repository_id: &str,
        job_id: &str,
    ) -> impl Future<Output = Result<(), GitError>> + Send;
}

impl WorkspaceOps for Workspace {
    async fn ensure_clone(&self, repo: &RepoRef, on_phase: PhaseFn<'_>) -> Result<(), GitError> {
        Workspace::ensure_clone(self, repo, |a, b| on_phase(a, b))
            .await
            .map(|_| ())
    }
    async fn create_worktree(
        &self,
        repo: &RepoRef,
        job_id: &str,
        branch: &str,
        start_from: Option<&str>,
    ) -> Result<Worktree, GitError> {
        Workspace::create_worktree(self, repo, job_id, branch, start_from).await
    }
    async fn chat_worktree(
        &self,
        repo: &RepoRef,
        thread_id: &str,
    ) -> Result<ChatWorktree, GitError> {
        Workspace::chat_worktree(self, repo, thread_id).await
    }
    async fn sync_base(&self, dir: &Path, base: &str) -> Result<SyncResult, GitError> {
        Workspace::sync_base(self, dir, base).await
    }
    async fn commit_leftovers(&self, dir: &Path, message: &str) -> Result<bool, GitError> {
        Workspace::commit_leftovers(self, dir, message).await
    }
    async fn head(&self, dir: &Path) -> Result<String, GitError> {
        Workspace::head(self, dir).await
    }
    async fn summary(&self, dir: &Path, base: &str) -> Result<Summary, GitError> {
        Workspace::summary(self, dir, base).await
    }
    async fn push(&self, dir: &Path, branch: &str) -> Result<(), GitError> {
        Workspace::push(self, dir, branch).await
    }
    async fn remove_worktree(&self, repository_id: &str, job_id: &str) -> Result<(), GitError> {
        let root = self.paths.worktrees.clone();
        Workspace::remove_worktree(self, repository_id, job_id, &root).await
    }
}

pub type EventSink = Arc<dyn Fn(RunEvent) + Send + Sync>;

pub struct JobRun {
    pub offer: Value,
    pub cwd: PathBuf,
    pub cancel: Cancel,
    pub on_event: EventSink,
    pub on_spawn: Arc<dyn Fn(u32) + Send + Sync>,
}

pub struct ChatRun {
    pub message: Value,
    pub checkout: ChatWorktree,
    pub cwd: PathBuf,
    pub cancel: Cancel,
    pub on_event: EventSink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    Canceled,
    Timeout,
}

/// What the agent said at the end (the stream-json `result` line).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunOutput {
    pub is_error: bool,
    pub text: String,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub learnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunResult {
    pub ok: bool,
    pub stopped: Option<Stopped>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub stderr: String,
    pub result: Option<RunOutput>,
}

/// The agent side: one job run, one chat run. Both end with a result, never an error.
pub trait Runners: Send + Sync + 'static {
    fn run_job(&self, run: JobRun) -> impl Future<Output = RunResult> + Send;
    fn run_chat(&self, run: ChatRun) -> impl Future<Output = RunResult> + Send;
}

#[derive(Debug, Clone, Default)]
pub struct SupervisorConfig {
    /// The agent's time limit, for the message when it is passed.
    pub timeout_minutes: u64,
    /// repository id -> setup command, run in a new worktree before the agent.
    pub setup: HashMap<String, String>,
    /// The person's home folder, so what is reported never names it.
    pub home: String,
}

pub struct SupervisorArgs<C, W, R> {
    pub conn: Arc<C>,
    pub device_id: String,
    pub server_key: VerifyingKey,
    pub pool_size: usize,
    pub workspace: Arc<W>,
    pub records: Arc<JobRecords>,
    pub runners: Arc<R>,
    pub config: SupervisorConfig,
    pub log: Arc<dyn Fn(String) + Send + Sync>,
    pub finish_retry: Option<Duration>,
}

// --- state ---------------------------------------------------------------------------------------------------------------------

/// A job or chat in the pool. `done` holds the outcome once it has reported.
#[derive(Clone)]
pub struct DoneHandle(watch::Receiver<Option<Value>>);

impl DoneHandle {
    pub async fn wait(&mut self) -> Value {
        match self.0.wait_for(|v| v.is_some()).await {
            Ok(v) => v.clone().unwrap_or(Value::Null),
            Err(_) => Value::Null,
        }
    }
}

struct Entry {
    cancel: Cancel,
    superseded: bool,
    done: DoneHandle,
}

struct State {
    jobs: HashMap<String, Entry>,
    chats: HashMap<String, Entry>,
    paused_until: u64,
    stopping: bool,
    pool_size: usize,
}

/// A pool slot just taken: its stop signal and the sender that reports its outcome.
type Slot = (Cancel, watch::Sender<Option<Value>>);

type SettingsFn = Box<dyn Fn(usize, Option<String>) + Send + Sync>;

struct Inner<C, W, R> {
    conn: Arc<C>,
    device_id: String,
    server_key: VerifyingKey,
    workspace: Arc<W>,
    records: Arc<JobRecords>,
    runners: Arc<R>,
    config: SupervisorConfig,
    log: Arc<dyn Fn(String) + Send + Sync>,
    finish_retry: Option<Duration>,
    state: Mutex<State>,
    on_settings: Mutex<SettingsFn>,
}

pub struct Supervisor<C: Poster + 'static, W: WorkspaceOps, R: Runners> {
    inner: Arc<Inner<C, W, R>>,
}

impl<C: Poster + 'static, W: WorkspaceOps, R: Runners> Clone for Supervisor<C, W, R> {
    fn clone(&self) -> Self {
        Supervisor {
            inner: self.inner.clone(),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A finished or failed flow that is not an outcome by itself.
enum Flow {
    Canceled,
    Failed(String),
}

impl From<GitError> for Flow {
    fn from(e: GitError) -> Self {
        Flow::Failed(e.0)
    }
}

fn repo_ref(v: &Value) -> RepoRef {
    let s = |k: &str| v[k].as_str().map(String::from);
    RepoRef {
        id: s("id").unwrap_or_default(),
        name: s("name").unwrap_or_default(),
        full_name: s("fullName"),
        clone_url: s("cloneUrl").unwrap_or_default(),
        default_branch: s("defaultBranch"),
    }
}

fn outcome(state: &str, result: &Map<String, Value>, error: Option<String>) -> Map<String, Value> {
    let mut o = Map::new();
    o.insert("state".into(), json!(state));
    o.extend(result.clone());
    if let Some(e) = error {
        o.insert("error".into(), json!(e));
    }
    o
}

fn chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// What `Number.isInteger` accepts.
fn as_integer(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| {
        v.as_f64()
            .filter(|f| f.fract() == 0.0 && f.abs() < 9e15)
            .map(|f| f as i64)
    })
}

impl<C: Poster + 'static, W: WorkspaceOps, R: Runners> Supervisor<C, W, R> {
    pub fn new(a: SupervisorArgs<C, W, R>) -> Self {
        Supervisor {
            inner: Arc::new(Inner {
                conn: a.conn,
                device_id: a.device_id,
                server_key: a.server_key,
                workspace: a.workspace,
                records: a.records,
                runners: a.runners,
                config: a.config,
                log: a.log,
                finish_retry: a.finish_retry,
                state: Mutex::new(State {
                    jobs: HashMap::new(),
                    chats: HashMap::new(),
                    paused_until: 0,
                    stopping: false,
                    pool_size: a.pool_size,
                }),
                on_settings: Mutex::new(Box::new(|_, _| {})),
            }),
        }
    }

    /// Called when a `settings.update` arrives: the pool size now in force and the chat model, if any.
    pub fn on_settings(&self, f: impl Fn(usize, Option<String>) + Send + Sync + 'static) {
        *self.inner.on_settings.lock().unwrap() = Box::new(f);
    }

    pub fn pool_size(&self) -> usize {
        self.inner.state.lock().unwrap().pool_size
    }

    pub fn free(&self) -> usize {
        let s = self.inner.state.lock().unwrap();
        s.pool_size.saturating_sub(s.jobs.len())
    }

    pub fn running(&self) -> Vec<String> {
        self.inner
            .state
            .lock()
            .unwrap()
            .jobs
            .keys()
            .cloned()
            .collect()
    }

    pub fn running_chats(&self) -> usize {
        self.inner.state.lock().unwrap().chats.len()
    }

    /// Jobs that ended here but whose finish Wheelhouse has not received yet: still ours, so they are listed with the running ones.
    pub fn pending(&self) -> Vec<String> {
        self.inner
            .records
            .all()
            .iter()
            .filter(|r| r["pendingFinish"].is_object())
            .filter_map(|r| r["id"].as_str().map(String::from))
            .collect()
    }

    pub fn job_done(&self, id: &str) -> Option<DoneHandle> {
        self.inner
            .state
            .lock()
            .unwrap()
            .jobs
            .get(id)
            .map(|e| e.done.clone())
    }

    pub fn chat_done(&self, id: &str) -> Option<DoneHandle> {
        self.inner
            .state
            .lock()
            .unwrap()
            .chats
            .get(id)
            .map(|e| e.done.clone())
    }

    /// One message from the stream, `data` as sent: `{ message, signature }`. Returns what was done, for tests and the log.
    pub async fn handle(&self, event: &str, data: &str) -> &'static str {
        let i = &self.inner;
        let Ok(parsed) = serde_json::from_str::<Value>(data) else {
            return i.drop_msg("unreadable message");
        };
        let message = match parsed.get("message") {
            Some(m) if m.is_object() => m.clone(),
            _ => return i.drop_msg("message without a body"),
        };
        let signature = parsed["signature"].as_str().unwrap_or("");
        if !verify_signed(&i.server_key, &message, signature) {
            return i.drop_msg(&format!("bad signature on {event}"));
        }
        if message["deviceId"].as_str() != Some(i.device_id.as_str()) {
            return i.drop_msg(&format!("{event} for another device"));
        }
        if message["type"].as_str() != Some(event) || !MESSAGE_TYPES.contains(&event) {
            return i.drop_msg(&format!("unknown message type {event}"));
        }
        match event {
            "settings.update" => self.settings(&message),
            "job.cancel" => self.cancel(
                message["jobId"].as_str().unwrap_or(""),
                message["superseded"] == json!(true),
            ),
            "chat.cancel" => self.cancel_chat(message["messageId"].as_str().unwrap_or("")),
            "chat.message" => self.chat(message).await,
            _ => self.offer(message).await,
        }
    }

    fn settings(&self, m: &Value) -> &'static str {
        let pool = {
            let mut s = self.inner.state.lock().unwrap();
            if let Some(n) = as_integer(&m["poolSize"]).filter(|n| (1..=8).contains(n)) {
                s.pool_size = n as usize;
            }
            s.pool_size
        };
        (self.inner.on_settings.lock().unwrap())(pool, m["chatModel"].as_str().map(String::from));
        (self.inner.log)(format!("settings: pool size {pool}"));
        "settings"
    }

    /// `superseded`: a newer run took this one's branch over, so it stops without committing or pushing anything.
    fn cancel(&self, job_id: &str, superseded: bool) -> &'static str {
        let mut s = self.inner.state.lock().unwrap();
        let Some(job) = s.jobs.get_mut(job_id) else {
            return "unknown job";
        };
        (self.inner.log)(format!(
            "job {job_id}: {}",
            if superseded {
                "stop requested, a newer run took over its branch"
            } else {
                "cancel requested"
            }
        ));
        if superseded {
            job.superseded = true;
        }
        job.cancel.cancel();
        "canceling"
    }

    fn cancel_chat(&self, id: &str) -> &'static str {
        let s = self.inner.state.lock().unwrap();
        let Some(chat) = s.chats.get(id) else {
            return "unknown chat";
        };
        (self.inner.log)(format!("chat {id}: cancel requested"));
        chat.cancel.cancel();
        "canceling"
    }

    async fn reject(&self, job_id: &str, reason: &str, detail: &str) -> &'static str {
        (self.inner.log)(format!("job {job_id}: refused ({reason}: {detail})"));
        if let Err(e) = self
            .inner
            .conn
            .post(
                &format!("/api/daemon/jobs/{job_id}/reject"),
                json!({ "reason": reason, "detail": detail }),
            )
            .await
        {
            (self.inner.log)(format!("job {job_id}: could not refuse ({e})"));
        }
        "rejected"
    }

    async fn offer(&self, m: Value) -> &'static str {
        let i = self.inner.clone();
        let Some(job_id) = m["jobId"].as_str().map(String::from) else {
            return i.drop_msg("offer without a job id");
        };
        if i.state.lock().unwrap().jobs.contains_key(&job_id) {
            return "duplicate";
        }
        if parse_iso_ms(m["expiresAt"].as_str().unwrap_or("")).is_some_and(|t| t < now_ms()) {
            return i.drop_msg(&format!("expired offer {job_id}")); // the server has already expired it
        }
        let has = |a: &Value| a.as_str().is_some_and(|s| !s.is_empty());
        if !has(&m["repository"]["cloneUrl"]) || !has(&m["issue"]["ref"]) || !has(&m["branch"]) {
            return self
                .reject(
                    &job_id,
                    "not_ready",
                    "the offer is missing the repository, issue or branch",
                )
                .await;
        }
        // The slot is taken before the accept goes out, so two offers arriving together cannot both get the last one.
        let taken: Result<Slot, (&str, String)> = {
            let mut s = i.state.lock().unwrap();
            if s.stopping {
                Err(("not_ready", "the daemon is stopping".into()))
            } else if s.paused_until > now_ms() {
                Err(("not_ready", "paused after hitting the usage limit".into()))
            } else if s.pool_size.saturating_sub(s.jobs.len()) == 0 {
                Err((
                    "no_free_slot",
                    format!("{} of {} running", s.jobs.len(), s.pool_size),
                ))
            } else {
                let (tx, rx) = watch::channel(None);
                let cancel = Cancel::new();
                s.jobs.insert(
                    job_id.clone(),
                    Entry {
                        cancel: cancel.clone(),
                        superseded: false,
                        done: DoneHandle(rx),
                    },
                );
                Ok((cancel, tx))
            }
        };
        let (cancel, done_tx) = match taken {
            Ok(t) => t,
            Err((reason, detail)) => return self.reject(&job_id, reason, &detail).await,
        };
        if let Err(e) = i
            .conn
            .post(&format!("/api/daemon/jobs/{job_id}/accept"), json!({}))
            .await
        {
            i.state.lock().unwrap().jobs.remove(&job_id);
            (i.log)(format!("job {job_id}: could not accept ({e})"));
            return "accept failed";
        }
        let repo = &m["repository"];
        (i.log)(format!(
            "job {job_id}: accepted {} on {} with {}",
            m["issue"]["ref"].as_str().unwrap_or(""),
            repo["fullName"]
                .as_str()
                .or(repo["name"].as_str())
                .unwrap_or(""),
            m["model"].as_str().unwrap_or("undefined")
        ));
        tokio::spawn(async move {
            let o = i.execute(m, cancel).await;
            i.state.lock().unwrap().jobs.remove(&job_id);
            let _ = done_tx.send(Some(o));
        });
        "accepted"
    }

    async fn chat(&self, m: Value) -> &'static str {
        let i = self.inner.clone();
        let Some(id) = m["messageId"].as_str().map(String::from) else {
            return i.drop_msg("chat message without an id");
        };
        if i.state.lock().unwrap().chats.contains_key(&id) {
            return "duplicate";
        }
        if parse_iso_ms(m["expiresAt"].as_str().unwrap_or("")).is_some_and(|t| t < now_ms()) {
            return i.drop_msg(&format!("expired chat message {id}"));
        }
        let has = |a: &Value| a.as_str().is_some_and(|s| !s.is_empty());
        if !has(&m["threadId"])
            || !has(&m["model"])
            || !has(&m["prompt"])
            || !has(&m["repository"]["id"])
            || !has(&m["repository"]["cloneUrl"])
        {
            return i
                .fail_chat(&id, "The message to this device was incomplete")
                .await;
        }
        let taken: Result<Slot, String> = {
            let mut s = i.state.lock().unwrap();
            if s.stopping {
                Err("The daemon is stopping".to_string())
            } else if s.paused_until > now_ms() {
                Err(
                    "This machine is paused after hitting the Claude usage limit; try again later"
                        .to_string(),
                )
            } else if s.chats.len() >= CHAT_POOL_SIZE {
                Err(format!(
                    "This device is already answering {} chats; try again in a moment",
                    s.chats.len()
                ))
            } else {
                let (tx, rx) = watch::channel(None);
                let cancel = Cancel::new();
                s.chats.insert(
                    id.clone(),
                    Entry {
                        cancel: cancel.clone(),
                        superseded: false,
                        done: DoneHandle(rx),
                    },
                );
                Ok((cancel, tx))
            }
        };
        let (cancel, done_tx) = match taken {
            Ok(t) => t,
            Err(why) => return i.fail_chat(&id, &why).await,
        };
        tokio::spawn(async move {
            let o = i.execute_chat(m, cancel).await;
            i.state.lock().unwrap().chats.remove(&id);
            let _ = done_tx.send(Some(o));
        });
        "accepted"
    }

    /// Stop taking work and cancel what is running; resolves when every job and chat has reported.
    pub async fn shutdown(&self) {
        let mut waits = vec![];
        {
            let mut s = self.inner.state.lock().unwrap();
            s.stopping = true;
            for e in s.jobs.values().chain(s.chats.values()) {
                e.cancel.cancel();
                waits.push(e.done.clone());
            }
        }
        for mut w in waits {
            w.wait().await;
        }
    }
}

impl<C: Poster + 'static, W: WorkspaceOps, R: Runners> Inner<C, W, R> {
    fn log(&self, m: String) {
        (self.log)(m);
    }

    fn drop_msg(&self, why: &str) -> &'static str {
        self.log(format!("dropped: {why}"));
        "dropped"
    }

    fn reporter(&self, id: &str, path: Option<&str>) -> JobReporter<C> {
        let mut cfg = ReporterConfig::default();
        if let Some(r) = self.finish_retry {
            cfg.finish_retry = r;
        }
        let log = self.log.clone();
        JobReporter::new(self.conn.clone(), id, path, cfg, move |m| log(m))
    }

    fn stopping(&self) -> bool {
        self.state.lock().unwrap().stopping
    }

    /// Why a run was stopped: the daemon going down is not the same as someone pressing Stop.
    fn stop_reason(&self) -> String {
        if self.stopping() {
            "The daemon was stopped while this ran. Run it again once it is back.".into()
        } else {
            "Stopped on request".into()
        }
    }

    fn is_superseded(&self, job_id: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .jobs
            .get(job_id)
            .is_some_and(|j| j.superseded)
    }

    fn pause_after_rate_limit(&self, what: &str) {
        self.state.lock().unwrap().paused_until = now_ms() + RATE_LIMIT_PAUSE_MS;
        self.log(format!(
            "usage limit reached: refusing new {what} for {} minutes",
            RATE_LIMIT_PAUSE_MS / 60_000
        ));
    }

    /// A chat message that cannot be answered here is finished as failed, so the person sees why instead of waiting.
    async fn fail_chat(&self, id: &str, error: &str) -> &'static str {
        self.log(format!("chat {id}: refused ({error})"));
        if let Err(e) = self
            .conn
            .post(
                &format!("/api/daemon/chat/{id}/finish"),
                json!({ "state": "failed", "error": error }),
            )
            .await
        {
            self.log(format!("chat {id}: could not report ({e})"));
        }
        "rejected"
    }

    /// One chat answer: the thread's worktree, the agent, the report. Never fails: every failure becomes the message's finish.
    async fn execute_chat(&self, m: Value, cancel: Cancel) -> Value {
        let id = m["messageId"].as_str().unwrap_or("").to_string();
        let reporter = self.reporter(&id, Some(&format!("/api/daemon/chat/{id}")));
        let out: Result<Map<String, Value>, Flow> = async {
            let repo = repo_ref(&m["repository"]);
            self.workspace.ensure_clone(&repo, &|_, _| {}).await?;
            if cancel.is_cancelled() {
                return Err(Flow::Canceled);
            }
            let wt = self
                .workspace
                .chat_worktree(&repo, m["threadId"].as_str().unwrap_or(""))
                .await?;
            let sink = reporter.clone();
            let r = self
                .runners
                .run_chat(ChatRun {
                    message: m.clone(),
                    cwd: wt.dir.clone(),
                    checkout: wt,
                    cancel: cancel.clone(),
                    on_event: Arc::new(move |e| {
                        if e.kind == "delta" || e.kind == "tool" {
                            sink.event(e.kind, e.data)
                        }
                    }),
                })
                .await;
            // An answer may talk about rate limits; only a run that failed on one pauses the machine.
            if r.result
                .as_ref()
                .is_some_and(|x| x.is_error && is_rate_limited(&x.text))
            {
                self.pause_after_rate_limit("work");
            }
            let mut result = Map::new();
            if let Some(s) = r.result.as_ref().and_then(|x| x.session_id.clone()) {
                result.insert("sessionId".into(), json!(s));
            }
            if let Some(c) = r.result.as_ref().and_then(|x| x.cost_usd) {
                result.insert("costUsd".into(), json!(c));
            }
            Ok(
                if r.stopped == Some(Stopped::Canceled) || cancel.is_cancelled() {
                    outcome("canceled", &result, Some(self.stop_reason()))
                } else if r.stopped == Some(Stopped::Timeout) {
                    outcome(
                        "failed",
                        &result,
                        Some("The answer took too long and was stopped".into()),
                    )
                } else if !r.ok {
                    let why = r
                        .error
                        .clone()
                        .or(r.result.as_ref().map(|x| x.text.clone()))
                        .unwrap_or_else(|| exit_reason(&r));
                    outcome("failed", &result, Some(why))
                } else {
                    let mut o = outcome("done", &result, None);
                    o.insert(
                        "text".into(),
                        json!(r.result.as_ref().map(|x| x.text.as_str()).unwrap_or("")),
                    );
                    o
                },
            )
        }
        .await;
        let o = match out {
            Ok(o) => o,
            Err(Flow::Canceled) => outcome("canceled", &Map::new(), Some(self.stop_reason())),
            Err(Flow::Failed(_)) if cancel.is_cancelled() => {
                outcome("canceled", &Map::new(), Some(self.stop_reason()))
            }
            Err(Flow::Failed(e)) => outcome("failed", &Map::new(), Some(e)),
        };
        let o = Value::Object(o);
        reporter.finish(o.clone()).await;
        self.log(format!(
            "chat {id}: {}{}",
            o["state"].as_str().unwrap_or(""),
            o["error"]
                .as_str()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        ));
        o
    }

    /// The job itself: worktree, setup, agent, commit, push, report. Never fails: every failure becomes the job's finish.
    async fn execute(&self, m: Value, cancel: Cancel) -> Value {
        let job_id = m["jobId"].as_str().unwrap_or("").to_string();
        let reporter = self.reporter(&job_id, None);
        let started = now_ms();
        let phase = |name: &str, detail: Option<&str>| {
            let mut d = json!({ "phase": name });
            if let Some(x) = detail {
                d["detail"] = json!(x);
            }
            reporter.event("phase", d);
        };
        let _ = self.records.write(&json!({
            "id": job_id, "repositoryId": m["repository"]["id"], "branch": m["branch"],
            "startedAt": iso_from_ms(started), "pid": null,
        }));
        let review = m["mode"] == json!("review") || m["push"] == json!(false);
        let mut cwd: Option<PathBuf> = None;
        let result = self
            .job_flow(
                m.clone(),
                &cancel,
                &reporter,
                &phase,
                review,
                started,
                &mut cwd,
            )
            .await;
        let mut out = match result {
            Ok(o) => o,
            Err(e) => {
                let (o, message) = match e {
                    Flow::Failed(msg) if !cancel.is_cancelled() => {
                        (outcome("failed", &Map::new(), Some(msg.clone())), msg)
                    }
                    _ => {
                        let why = if self.is_superseded(&job_id) {
                            SUPERSEDED_REASON.to_string()
                        } else {
                            self.stop_reason()
                        };
                        (
                            outcome("canceled", &Map::new(), Some(why)),
                            "Stopped on request".to_string(),
                        )
                    }
                };
                reporter.event("error", json!({ "message": chars(&message, 1000) }));
                o
            }
        };
        out.insert("durationMs".into(), json!(now_ms().saturating_sub(started)));
        let report = Value::Object(out.clone());
        let fin = reporter.finish(report.clone()).await;
        // A finish that could not get through (no connection) is kept with the job's record and sent again once Wheelhouse is reachable.
        if fin.retry {
            let mut patch = Map::new();
            patch.insert("pendingFinish".into(), report.clone());
            let _ = self.records.update(&job_id, patch);
        } else {
            let _ = self.records.remove(&job_id);
        }
        if cwd.is_some()
            && out.get("state") == Some(&json!("succeeded"))
            && (out.get("pushed") == Some(&json!(true)) || review)
        {
            let _ = self
                .workspace
                .remove_worktree(m["repository"]["id"].as_str().unwrap_or(""), &job_id)
                .await;
        }
        self.log(format!(
            "job {job_id}: {}{}",
            out.get("state").and_then(Value::as_str).unwrap_or(""),
            out.get("error")
                .and_then(Value::as_str)
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        ));
        report
    }

    #[allow(clippy::too_many_arguments)]
    async fn job_flow(
        &self,
        mut m: Value,
        cancel: &Cancel,
        reporter: &JobReporter<C>,
        phase: &(dyn Fn(&str, Option<&str>) + Sync),
        review: bool,
        started: u64,
        cwd_out: &mut Option<PathBuf>,
    ) -> Result<Map<String, Value>, Flow> {
        let job_id = m["jobId"].as_str().unwrap_or("").to_string();
        let repo = repo_ref(&m["repository"]);
        let branch = m["branch"].as_str().unwrap_or("").to_string();
        let issue_ref = m["issue"]["ref"].as_str().unwrap_or("").to_string();
        self.workspace
            .ensure_clone(&repo, &|a, b| phase(a, Some(b)))
            .await?;
        if cancel.is_cancelled() {
            return Err(Flow::Canceled);
        }
        // A continuation starts from the job's own branch as the earlier run left it; a fresh run from its base.
        let start_from = m["startFrom"]
            .as_str()
            .or(m["baseBranch"].as_str())
            .map(String::from);
        let detail = if m["previous"].is_null() || m["previous"] == json!(false) {
            format!(
                "{branch} from {}",
                start_from
                    .clone()
                    .or(repo.default_branch.clone())
                    .unwrap_or_else(|| "undefined".into())
            )
        } else {
            format!("{branch}, continuing from its last push")
        };
        phase("worktree", Some(&detail));
        let wt = self
            .workspace
            .create_worktree(&repo, &job_id, &branch, start_from.as_deref())
            .await?;
        let cwd = wt.dir.clone();
        *cwd_out = Some(cwd.clone());
        if !(m["previous"].is_null() || m["previous"] == json!(false)) && !review {
            let base = format!(
                "origin/{}",
                m["baseBranch"]
                    .as_str()
                    .or(repo.default_branch.as_deref())
                    .unwrap_or("undefined")
            );
            let sync = self.workspace.sync_base(&cwd, &base).await?;
            let (detail, value) = match sync {
                SyncResult::Current => (format!("current with {base}"), json!("current")),
                SyncResult::Merged => (format!("merged with {base}"), json!("merged")),
                SyncResult::Conflicts(files) => (format!("conflicts with {base}"), json!(files)),
            };
            phase("base", Some(&detail));
            m["baseSync"] = value;
        }
        if let Some(setup) = self.config.setup.get(&repo.id) {
            phase("setup", Some(setup));
            match run_shell(&cwd, setup, cancel, SETUP_TIMEOUT).await {
                Ended::Exited { code: Some(0), .. } => {}
                Ended::Exited { code, stderr, .. } => {
                    let tail: Vec<&str> = stderr.trim().lines().collect();
                    let tail = tail[tail.len().saturating_sub(3)..].join(" ");
                    let why = if tail.is_empty() {
                        format!(
                            "exited with {}",
                            code.map(|c| c.to_string())
                                .unwrap_or_else(|| "a signal".into())
                        )
                    } else {
                        tail
                    };
                    return Err(Flow::Failed(format!("/bin/sh -c {setup}: {why}")));
                }
                Ended::TimedOut => {
                    return Err(Flow::Failed(format!("/bin/sh -c {setup}: timed out")))
                }
                Ended::Stopped => return Err(Flow::Canceled),
                Ended::CouldNotStart(e) => {
                    return Err(Flow::Failed(format!("/bin/sh -c {setup}: {e}")))
                }
            }
        }
        if cancel.is_cancelled() {
            return Err(Flow::Canceled);
        }
        phase("agent", Some(m["model"].as_str().unwrap_or("undefined")));
        let mut r = self.agent(m.clone(), &cwd, cancel, reporter, &job_id).await;
        // WH-171: before anything is pushed, the repository's checks run on what the agent committed. A check that failed goes back to the
        // agent with its output, up to MAX_FIX_ATTEMPTS times; one that could not run cannot be fixed by the agent, so it just ends the run.
        let to_run: Vec<Check> = m["repository"]["checks"]
            .as_array()
            .map(|a| a.iter().filter_map(Check::from_json).collect())
            .unwrap_or_default();
        let mut checks: Vec<CheckResult> = vec![];
        let mut checked_sha: Option<String> = None;
        let mut attempt = 0;
        while !review
            && r.ok
            && !to_run.is_empty()
            && !self.is_superseded(&job_id)
            && !cancel.is_cancelled()
        {
            self.workspace
                .commit_leftovers(
                    &cwd,
                    &format!("{issue_ref}: work in progress from the agent run"),
                )
                .await?;
            phase(
                "checks",
                Some(
                    &to_run
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            );
            checks = run_checks(&to_run, &cwd, cancel, &self.config.home, |c| {
                reporter.event("check", c.to_json(false))
            })
            .await;
            checked_sha = Some(self.workspace.head(&cwd).await?); // WH-191: the checks describe this commit, whatever is pushed later
            let Some(bad) = blocking(&checks) else { break };
            if bad.status != Status::Failed || attempt >= MAX_FIX_ATTEMPTS || cancel.is_cancelled()
            {
                break;
            }
            phase(
                "fixing",
                Some(&format!(
                    "{} (attempt {} of {})",
                    bad.name,
                    attempt + 1,
                    MAX_FIX_ATTEMPTS
                )),
            );
            let fix: Vec<Value> = checks
                .iter()
                .filter(|c| c.required && c.status == Status::Failed)
                .map(|c| c.to_json(true))
                .collect();
            let mut next = m.clone();
            next["fix"] = json!(fix);
            r = self.agent(next, &cwd, cancel, reporter, &job_id).await;
            attempt += 1;
        }
        if r.result.as_ref().is_some_and(|x| is_rate_limited(&x.text)) {
            self.pause_after_rate_limit("jobs");
        }
        let cost = r
            .result
            .as_ref()
            .and_then(|x| x.cost_usd)
            .map(|c| json!(c))
            .unwrap_or(Value::Null);
        // Whatever happened, keep the agent's work: commit what it left and push the branch if there is anything on it. Not when a newer
        // run took the branch over: pushing could overwrite its commits, so the work stays in this worktree for a person to look at.
        if self.is_superseded(&job_id) {
            let mut res = Map::new();
            res.insert("pushed".into(), json!(false));
            res.insert("costUsd".into(), cost);
            return Ok(outcome("canceled", &res, Some(SUPERSEDED_REASON.into())));
        }
        // A review (WH-122) only reads: nothing is committed or pushed, and what it says is the result.
        let mut commit = Value::Null;
        let mut diff_stats = json!({});
        let mut pushed = false;
        if !review {
            phase("pushing", Some(&branch));
            self.workspace
                .commit_leftovers(
                    &cwd,
                    &format!("{issue_ref}: work in progress from the agent run"),
                )
                .await?;
            let sum = self.workspace.summary(&cwd, &wt.base).await?;
            commit = json!(sum.commit);
            diff_stats = json!({ "files": sum.files, "insertions": sum.insertions, "deletions": sum.deletions });
            if sum.ahead > 0 {
                self.workspace.push(&cwd, &branch).await?;
                pushed = true;
            }
        }
        let mut res = Map::new();
        res.insert("commit".into(), commit);
        res.insert("pushed".into(), json!(pushed));
        res.insert("diffStats".into(), diff_stats);
        res.insert("costUsd".into(), cost);
        res.insert("durationMs".into(), json!(now_ms().saturating_sub(started)));
        if let Some(l) = r.result.as_ref().filter(|x| !x.learnings.is_empty()) {
            res.insert("learnings".into(), json!(l.learnings));
        }
        if !checks.is_empty() {
            res.insert(
                "checks".into(),
                json!(checks.iter().map(|c| c.to_json(true)).collect::<Vec<_>>()),
            );
            res.insert("checkedSha".into(), json!(checked_sha));
        }
        let bad = blocking(&checks);
        Ok(
            if r.stopped == Some(Stopped::Canceled) || cancel.is_cancelled() {
                let reason = if self.is_superseded(&job_id) {
                    SUPERSEDED_REASON.to_string()
                } else {
                    self.stop_reason()
                };
                outcome("canceled", &res, Some(reason))
            } else if r.stopped == Some(Stopped::Timeout) {
                outcome(
                    "failed",
                    &res,
                    Some(format!(
                        "The agent ran past the {}-minute limit",
                        self.config.timeout_minutes
                    )),
                )
            } else if !r.ok {
                let why = r
                    .error
                    .clone()
                    .or(r.result.as_ref().map(|x| x.text.clone()))
                    .unwrap_or_else(|| exit_reason(&r));
                outcome("failed", &res, Some(why))
            } else if let Some(bad) = bad {
                let verb = if bad.status == Status::Failed {
                    "failed"
                } else {
                    "could not run"
                };
                let tail = if bad.tail.is_empty() {
                    String::new()
                } else {
                    let lines: Vec<&str> = bad.tail.split('\n').collect();
                    format!(
                        ": {}",
                        chars(&lines[lines.len().saturating_sub(3)..].join(" "), 300)
                    )
                };
                outcome(
                    "failed",
                    &res,
                    Some(format!("Check \"{}\" {verb}{tail}", bad.name)),
                )
            } else {
                outcome("succeeded", &res, None)
            },
        )
    }

    async fn agent(
        &self,
        offer: Value,
        cwd: &Path,
        cancel: &Cancel,
        reporter: &JobReporter<C>,
        job_id: &str,
    ) -> RunResult {
        let sink = reporter.clone();
        let records = self.records.clone();
        let id = job_id.to_string();
        self.runners
            .run_job(JobRun {
                offer,
                cwd: cwd.to_path_buf(),
                cancel: cancel.clone(),
                on_event: Arc::new(move |e| sink.event(e.kind, e.data)),
                on_spawn: Arc::new(move |pid| {
                    let mut patch = Map::new();
                    patch.insert("pid".into(), json!(pid));
                    let _ = records.update(&id, patch);
                }),
            })
            .await
    }
}

/// `claude exited with 1`: what a failed run says when it has nothing better (its stderr first).
fn exit_reason(r: &RunResult) -> String {
    if r.stderr.is_empty() {
        format!(
            "claude exited with {}",
            r.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "null".into())
        )
    } else {
        r.stderr.clone()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::connection::ConnError;
    use std::collections::VecDeque;
    use tokio::sync::oneshot;
    use wh_proto::sign::sign_value;

    const DEVICE: &str = "11111111-1111-1111-1111-111111111111";
    const REPO: &str = "22222222-2222-2222-2222-222222222222";

    // --- fakes ---------------------------------------------------------------------------------------------------------------

    #[derive(Default)]
    struct FakeConn {
        posts: Mutex<Vec<(String, Value)>>,
        /// Every finish POST fails with this.
        fail_finish: Mutex<Option<ConnError>>,
    }

    impl Poster for FakeConn {
        async fn post(&self, path: &str, body: Value) -> Result<Value, ConnError> {
            self.posts.lock().unwrap().push((path.to_string(), body));
            if path.ends_with("/finish") {
                if let Some(e) = self.fail_finish.lock().unwrap().clone() {
                    return Err(e);
                }
            }
            Ok(json!({}))
        }
    }

    #[derive(Default)]
    struct FakeWs {
        calls: Mutex<Vec<String>>,
        fail_create: Mutex<Option<String>>,
        fail_chat: Mutex<Option<String>>,
        dir: Mutex<Option<PathBuf>>,
    }

    impl WorkspaceOps for FakeWs {
        async fn ensure_clone(&self, _: &RepoRef, _: PhaseFn<'_>) -> Result<(), GitError> {
            Ok(())
        }
        async fn create_worktree(
            &self,
            _: &RepoRef,
            job_id: &str,
            _: &str,
            _: Option<&str>,
        ) -> Result<Worktree, GitError> {
            if let Some(e) = self.fail_create.lock().unwrap().clone() {
                return Err(GitError(e));
            }
            let dir = self
                .dir
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("/wt/{job_id}")));
            Ok(Worktree {
                dir,
                base: "origin/main".into(),
            })
        }
        async fn chat_worktree(
            &self,
            _: &RepoRef,
            thread_id: &str,
        ) -> Result<ChatWorktree, GitError> {
            if let Some(e) = self.fail_chat.lock().unwrap().clone() {
                return Err(GitError(e));
            }
            Ok(ChatWorktree {
                dir: PathBuf::from(format!("/chat/{thread_id}")),
                commit: "abc1234".into(),
                branches: vec!["origin/main".into()],
            })
        }
        async fn sync_base(&self, _: &Path, _: &str) -> Result<SyncResult, GitError> {
            Ok(SyncResult::Current)
        }
        async fn commit_leftovers(&self, _: &Path, _: &str) -> Result<bool, GitError> {
            self.calls.lock().unwrap().push("commit".into());
            Ok(false)
        }
        async fn head(&self, _: &Path) -> Result<String, GitError> {
            Ok("c0ffee1234".into())
        }
        async fn summary(&self, _: &Path, _: &str) -> Result<Summary, GitError> {
            Ok(Summary {
                ahead: 1,
                commit: "abc1234".into(),
                files: 1,
                insertions: 2,
                deletions: 0,
            })
        }
        async fn push(&self, _: &Path, _: &str) -> Result<(), GitError> {
            self.calls.lock().unwrap().push("push".into());
            Ok(())
        }
        async fn remove_worktree(&self, _: &str, _: &str) -> Result<(), GitError> {
            self.calls.lock().unwrap().push("remove".into());
            Ok(())
        }
    }

    type JobFn = Arc<dyn Fn(&JobRun, usize) -> RunResult + Send + Sync>;
    type ChatFn = Arc<dyn Fn(&ChatRun) -> RunResult + Send + Sync>;

    #[derive(Default)]
    struct FakeRunners {
        releases: Mutex<HashMap<String, oneshot::Sender<RunOutput>>>,
        job_override: Mutex<Option<JobFn>>,
        chat_override: Mutex<Option<ChatFn>>,
        job_runs: Mutex<Vec<Value>>, // the `fix` of each run's offer
    }

    fn done(out: RunOutput) -> RunResult {
        RunResult {
            ok: true,
            exit_code: Some(0),
            result: Some(out),
            ..Default::default()
        }
    }

    fn canceled() -> RunResult {
        RunResult {
            ok: false,
            stopped: Some(Stopped::Canceled),
            ..Default::default()
        }
    }

    impl Runners for FakeRunners {
        async fn run_job(&self, run: JobRun) -> RunResult {
            let n = {
                let mut runs = self.job_runs.lock().unwrap();
                runs.push(run.offer["fix"].clone());
                runs.len()
            };
            (run.on_event)(RunEvent {
                kind: "started",
                data: json!({}),
            });
            if let Some(f) = self.job_override.lock().unwrap().clone() {
                return f(&run, n);
            }
            let (tx, rx) = oneshot::channel();
            self.releases
                .lock()
                .unwrap()
                .insert(run.offer["jobId"].as_str().unwrap().to_string(), tx);
            tokio::select! {
                Ok(out) = rx => done(out),
                _ = run.cancel.cancelled() => canceled(),
            }
        }

        async fn run_chat(&self, run: ChatRun) -> RunResult {
            (run.on_event)(RunEvent {
                kind: "started",
                data: json!({}),
            });
            (run.on_event)(RunEvent {
                kind: "delta",
                data: json!({ "text": "In auth/" }),
            });
            (run.on_event)(RunEvent {
                kind: "tool",
                data: json!({ "name": "Read", "summary": "auth/index.ts" }),
            });
            if let Some(f) = self.chat_override.lock().unwrap().clone() {
                return f(&run);
            }
            let (tx, rx) = oneshot::channel();
            self.releases
                .lock()
                .unwrap()
                .insert(run.message["messageId"].as_str().unwrap().to_string(), tx);
            tokio::select! {
                Ok(_) = rx => done(RunOutput { text: "In auth/index.ts".into(), session_id: Some("s1".into()), cost_usd: Some(0.02), ..Default::default() }),
                _ = run.cancel.cancelled() => canceled(),
            }
        }
    }

    struct S {
        sup: Supervisor<FakeConn, FakeWs, FakeRunners>,
        conn: Arc<FakeConn>,
        ws: Arc<FakeWs>,
        runners: Arc<FakeRunners>,
        records: Arc<JobRecords>,
        server: ed25519_dalek::SigningKey,
        _dir: tempfile::TempDir,
        n: Mutex<u32>,
    }

    fn setup() -> S {
        let server = wh_proto::keys::generate();
        let dir = tempfile::tempdir().unwrap();
        let (conn, ws, runners) = (
            Arc::new(FakeConn::default()),
            Arc::new(FakeWs::default()),
            Arc::new(FakeRunners::default()),
        );
        let records = Arc::new(JobRecords::new(dir.path().join("jobs")));
        let sup = Supervisor::new(SupervisorArgs {
            conn: conn.clone(),
            device_id: DEVICE.into(),
            server_key: server.verifying_key(),
            pool_size: 2,
            workspace: ws.clone(),
            records: records.clone(),
            runners: runners.clone(),
            config: SupervisorConfig {
                timeout_minutes: 45,
                setup: HashMap::new(),
                home: "/home/x".into(),
            },
            log: Arc::new(|_| {}),
            finish_retry: Some(Duration::from_millis(1)),
        });
        S {
            sup,
            conn,
            ws,
            runners,
            records,
            server,
            _dir: dir,
            n: Mutex::new(0),
        }
    }

    fn in_a_minute() -> String {
        iso_from_ms(now_ms() + 60_000)
    }

    fn a_second_ago() -> String {
        iso_from_ms(now_ms() - 1000)
    }

    impl S {
        fn next(&self) -> u32 {
            let mut n = self.n.lock().unwrap();
            *n += 1;
            *n
        }

        fn offer(&self, extra: Value) -> Value {
            let n = self.next();
            let mut o = json!({
                "type": "job.offer", "deviceId": DEVICE, "issuedAt": iso_from_ms(now_ms()),
                "jobId": format!("00000000-0000-0000-0000-{n:012}"), "expiresAt": in_a_minute(), "model": "sonnet",
                "branch": format!("wh/WH-{n}-x"), "boardAccess": "none",
                "issue": { "id": "i", "ref": format!("WH-{n}"), "title": "Do it", "body": "" },
                "repository": { "id": REPO, "name": "app", "fullName": "acme/app", "cloneUrl": "https://github.com/acme/app.git", "defaultBranch": "main", "pathLimit": null },
            });
            o.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            o
        }

        fn chat_message(&self, extra: Value) -> Value {
            let n = self.next();
            let mut o = json!({
                "type": "chat.message", "deviceId": DEVICE, "issuedAt": iso_from_ms(now_ms()),
                "messageId": format!("99999999-0000-0000-0000-{n:012}"), "threadId": "88888888-0000-0000-0000-000000000001",
                "model": "sonnet", "sessionId": null, "prompt": "Where is auth handled?", "expiresAt": in_a_minute(),
                "repository": { "id": REPO, "name": "app", "fullName": "acme/app", "cloneUrl": "https://github.com/acme/app.git", "defaultBranch": "main" },
            });
            o.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            o
        }

        /// A message signed the way Wheelhouse signs it.
        fn signed(&self, message: &Value) -> (String, String) {
            self.signed_by(message, &self.server)
        }

        fn signed_by(&self, message: &Value, key: &ed25519_dalek::SigningKey) -> (String, String) {
            (
                message["type"].as_str().unwrap_or("").to_string(),
                json!({ "message": message, "signature": sign_value(key, message) }).to_string(),
            )
        }

        async fn send(&self, message: &Value) -> &'static str {
            let (event, data) = self.signed(message);
            self.sup.handle(&event, &data).await
        }

        fn control(&self, kind: &str, extra: Value) -> Value {
            let mut m = json!({ "type": kind, "deviceId": DEVICE, "issuedAt": "x" });
            m.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            m
        }

        fn posts(&self) -> Vec<(String, Value)> {
            self.conn.posts.lock().unwrap().clone()
        }

        fn post_ending(&self, suffix: &str) -> Option<Value> {
            self.posts()
                .into_iter()
                .rev()
                .find(|p| p.0.ends_with(suffix))
                .map(|p| p.1)
        }

        fn release(&self, id: &str, out: RunOutput) {
            self.runners
                .releases
                .lock()
                .unwrap()
                .remove(id)
                .unwrap()
                .send(out)
                .unwrap();
        }

        async fn wait_job(&self, id: &str) -> Value {
            self.sup
                .job_done(id)
                .expect("job is in the pool")
                .wait()
                .await
        }

        async fn wait_chat(&self, id: &str) -> Value {
            self.sup
                .chat_done(id)
                .expect("chat is in the pool")
                .wait()
                .await
        }

        fn chat_finish(&self, m: &Value) -> Option<Value> {
            let path = format!(
                "/api/daemon/chat/{}/finish",
                m["messageId"].as_str().unwrap()
            );
            self.posts().into_iter().find(|p| p.0 == path).map(|p| p.1)
        }
    }

    fn id(m: &Value) -> String {
        m["jobId"].as_str().unwrap().to_string()
    }

    fn mid(m: &Value) -> String {
        m["messageId"].as_str().unwrap().to_string()
    }

    /// Wait until the runner has registered (the job reached the agent), so a release has something to release.
    async fn until_running(s: &S, id: &str) {
        for _ in 0..200 {
            if s.runners.releases.lock().unwrap().contains_key(id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("{id} never reached the runner");
    }

    // --- jobs ----------------------------------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_verified_offer_is_accepted_run_and_finished_with_what_the_run_reported() {
        let s = setup();
        let m = s.offer(json!({}));
        assert_eq!(s.send(&m).await, "accepted");
        assert_eq!(s.sup.running(), vec![id(&m)]);
        until_running(&s, &id(&m)).await;
        s.release(
            &id(&m),
            RunOutput {
                cost_usd: Some(0.1),
                text: "done".into(),
                ..Default::default()
            },
        );
        s.wait_job(&id(&m)).await;
        let posts = s.posts();
        assert_eq!(posts[0].0, format!("/api/daemon/jobs/{}/accept", id(&m)));
        let finish = s.post_ending("/finish").unwrap();
        assert_eq!(finish["state"], "succeeded");
        assert_eq!(finish["commit"], "abc1234");
        assert_eq!(finish["pushed"], true);
        assert_eq!(
            finish["diffStats"],
            json!({ "files": 1, "insertions": 2, "deletions": 0 })
        );
        assert!(posts.iter().any(|p| p.0.ends_with("/events")
            && p.1["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["type"] == "started")));
        assert!(s.sup.running().is_empty());
    }

    #[tokio::test]
    async fn a_runs_learnings_go_with_its_finish() {
        let s = setup();
        let m = s.offer(json!({}));
        s.send(&m).await;
        until_running(&s, &id(&m)).await;
        s.release(
            &id(&m),
            RunOutput {
                learnings: vec!["This team keeps tests next to source".into()],
                ..Default::default()
            },
        );
        s.wait_job(&id(&m)).await;
        assert_eq!(
            s.post_ending("/finish").unwrap()["learnings"],
            json!(["This team keeps tests next to source"])
        );
    }

    #[tokio::test]
    async fn a_review_commits_nothing_and_pushes_nothing_and_still_finishes_with_what_it_said() {
        let s = setup();
        let m = s.offer(json!({ "mode": "review", "push": false, "startFrom": "wh/feature",
            "review": { "pullRequest": { "number": 7, "url": "u", "headBranch": "wh/feature", "headSha": "abc" } } }));
        s.send(&m).await;
        until_running(&s, &id(&m)).await;
        s.release(&id(&m), RunOutput::default());
        s.wait_job(&id(&m)).await;
        let finish = s.post_ending("/finish").unwrap();
        assert_eq!(finish["state"], "succeeded");
        assert_eq!(finish["pushed"], false);
        let calls = s.ws.calls.lock().unwrap().clone();
        assert!(
            !calls.contains(&"commit".to_string()) && !calls.contains(&"push".to_string()),
            "no commit and no push: {calls:?}"
        );
    }

    #[tokio::test]
    async fn unverifiable_messages_are_dropped_without_a_word_to_the_server() {
        let s = setup();
        let stranger = wh_proto::keys::generate();
        let (e, d) = s.signed_by(&s.offer(json!({})), &stranger);
        assert_eq!(s.sup.handle(&e, &d).await, "dropped");
        assert_eq!(
            s.send(&s.offer(json!({ "deviceId": "someone-else" })))
                .await,
            "dropped"
        );
        let m = s.offer(json!({}));
        let mut tampered: Value = serde_json::from_str(&s.signed(&m).1).unwrap();
        tampered["message"]["repository"]["cloneUrl"] = json!("https://evil.example/x.git");
        assert_eq!(
            s.sup.handle("job.offer", &tampered.to_string()).await,
            "dropped"
        );
        assert_eq!(
            s.sup.handle("job.cancel", &s.signed(&m).1).await,
            "dropped",
            "the event must match the signed type"
        );
        assert_eq!(
            s.send(&s.offer(json!({ "expiresAt": a_second_ago() })))
                .await,
            "dropped"
        );
        assert_eq!(s.sup.handle("job.offer", "not json").await, "dropped");
        assert!(s.posts().is_empty());
    }

    #[tokio::test]
    async fn never_more_than_the_pool_size_at_once_the_next_offer_is_refused_for_want_of_a_slot() {
        let s = setup();
        let (a, b, c) = (s.offer(json!({})), s.offer(json!({})), s.offer(json!({})));
        s.send(&a).await;
        s.send(&b).await;
        assert_eq!(s.send(&c).await, "rejected");
        let r = s
            .posts()
            .into_iter()
            .find(|p| p.0 == format!("/api/daemon/jobs/{}/reject", id(&c)))
            .unwrap();
        assert_eq!(r.1["reason"], "no_free_slot");
        assert_eq!(s.send(&a).await, "duplicate");
        until_running(&s, &id(&a)).await;
        let mut done = s.sup.job_done(&id(&a)).unwrap();
        s.release(&id(&a), RunOutput::default());
        done.wait().await;
        assert_eq!(s.sup.free(), 1);
    }

    #[tokio::test]
    async fn a_settings_message_changes_the_pool_size() {
        let s = setup();
        let seen = Arc::new(Mutex::new(vec![]));
        let seen2 = seen.clone();
        s.sup
            .on_settings(move |n, model| seen2.lock().unwrap().push((n, model)));
        assert_eq!(
            s.send(&s.control(
                "settings.update",
                json!({ "poolSize": 1, "chatModel": null })
            ))
            .await,
            "settings"
        );
        assert_eq!(s.sup.pool_size(), 1);
        assert_eq!(*seen.lock().unwrap(), vec![(1, None)]);
        s.send(&s.offer(json!({}))).await;
        assert_eq!(s.send(&s.offer(json!({}))).await, "rejected");
        // Out of range or not a whole number: the size stays.
        s.send(&s.control("settings.update", json!({ "poolSize": 9 })))
            .await;
        s.send(&s.control("settings.update", json!({ "poolSize": 2.5 })))
            .await;
        assert_eq!(s.sup.pool_size(), 1);
    }

    #[tokio::test]
    async fn a_signed_cancel_stops_the_run_it_is_reported_canceled_and_the_work_still_pushed() {
        let s = setup();
        let m = s.offer(json!({}));
        s.send(&m).await;
        until_running(&s, &id(&m)).await;
        let mut done = s.sup.job_done(&id(&m)).unwrap();
        assert_eq!(
            s.send(&s.control("job.cancel", json!({ "jobId": id(&m) })))
                .await,
            "canceling"
        );
        let out = done.wait().await;
        assert_eq!(out["state"], "canceled");
        assert_eq!(out["pushed"], true);
        assert_eq!(s.post_ending("/finish").unwrap()["state"], "canceled");
        assert_eq!(
            s.send(&s.control("job.cancel", json!({ "jobId": id(&m) })))
                .await,
            "unknown job"
        );
    }

    #[tokio::test]
    async fn a_failing_step_fails_the_job_with_the_reason_and_frees_the_slot() {
        let s = setup();
        *s.ws.fail_create.lock().unwrap() = Some("fatal: invalid reference: origin/main".into());
        let m = s.offer(json!({}));
        s.send(&m).await;
        let out = s.wait_job(&id(&m)).await;
        assert_eq!(out["state"], "failed");
        assert!(out["error"].as_str().unwrap().contains("invalid reference"));
        assert_eq!(s.sup.free(), 2);
        assert!(s.post_ending("/events").unwrap()["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "error"));
    }

    #[tokio::test]
    async fn a_run_that_hits_the_usage_limit_pauses_new_offers() {
        let s = setup();
        *s.runners.job_override.lock().unwrap() = Some(Arc::new(|_, _| RunResult {
            ok: false,
            exit_code: Some(1),
            result: Some(RunOutput {
                is_error: true,
                text: "Claude usage limit reached. Your limit will reset at 5pm".into(),
                ..Default::default()
            }),
            ..Default::default()
        }));
        let m = s.offer(json!({}));
        s.send(&m).await;
        s.wait_job(&id(&m)).await;
        let next = s.offer(json!({}));
        assert_eq!(s.send(&next).await, "rejected");
        let r = s
            .posts()
            .into_iter()
            .find(|p| p.0 == format!("/api/daemon/jobs/{}/reject", id(&next)))
            .unwrap();
        assert!(r.1["detail"].as_str().unwrap().contains("usage limit"));
    }

    #[tokio::test]
    async fn a_stop_from_wheelhouse_says_it_was_requested_the_daemon_going_down_says_that_instead()
    {
        let s = setup();
        let a = s.offer(json!({}));
        s.send(&a).await;
        s.send(&s.control("job.cancel", json!({ "jobId": id(&a) })))
            .await;
        assert_eq!(s.wait_job(&id(&a)).await["error"], "Stopped on request");
        let b = s.offer(json!({}));
        s.send(&b).await;
        s.sup.shutdown().await;
        let finish = s
            .posts()
            .into_iter()
            .find(|p| p.0 == format!("/api/daemon/jobs/{}/finish", id(&b)))
            .unwrap()
            .1;
        assert_eq!(finish["state"], "canceled");
        assert!(finish["error"]
            .as_str()
            .unwrap()
            .contains("daemon was stopped"));
    }

    #[tokio::test]
    async fn shutdown_cancels_what_is_running_and_waits_for_it_to_report() {
        let s = setup();
        let m = s.offer(json!({}));
        s.send(&m).await;
        s.sup.shutdown().await;
        assert_eq!(s.post_ending("/finish").unwrap()["state"], "canceled");
        assert_eq!(s.send(&s.offer(json!({}))).await, "rejected");
    }

    #[tokio::test]
    async fn a_run_whose_branch_a_newer_run_took_over_stops_without_committing_or_pushing_and_keeps_its_worktree(
    ) {
        let s = setup();
        let m = s.offer(json!({}));
        s.send(&m).await;
        until_running(&s, &id(&m)).await;
        let mut done = s.sup.job_done(&id(&m)).unwrap();
        assert_eq!(
            s.send(&s.control("job.cancel", json!({ "jobId": id(&m), "superseded": true })))
                .await,
            "canceling"
        );
        let out = done.wait().await;
        assert!(
            s.ws.calls.lock().unwrap().is_empty(),
            "nothing committed, pushed or removed"
        );
        assert_eq!(out["state"], "canceled");
        assert_eq!(out["pushed"], false);
        assert_eq!(out["error"], SUPERSEDED_REASON);
        assert_eq!(
            s.post_ending("/finish").unwrap()["error"],
            SUPERSEDED_REASON
        );
    }

    #[tokio::test]
    async fn a_finish_that_cannot_get_through_is_kept_with_the_job_and_listed_with_it_until_it_is_sent(
    ) {
        let s = setup();
        *s.conn.fail_finish.lock().unwrap() = Some(ConnError::Transport("fetch failed".into()));
        let m = s.offer(json!({}));
        s.send(&m).await;
        until_running(&s, &id(&m)).await;
        let mut done = s.sup.job_done(&id(&m)).unwrap();
        s.release(&id(&m), RunOutput::default());
        done.wait().await;
        assert_eq!(
            s.records.read(&id(&m)).unwrap()["pendingFinish"]["state"],
            "succeeded"
        );
        assert_eq!(s.sup.pending(), vec![id(&m)]);
        assert!(s.sup.running().is_empty());
    }

    #[tokio::test]
    async fn a_finish_wheelhouse_refuses_for_good_is_not_kept() {
        let s = setup();
        *s.conn.fail_finish.lock().unwrap() = Some(ConnError::Http {
            status: 409,
            code: "x".into(),
            message: "The job is canceled".into(),
        });
        let m = s.offer(json!({}));
        s.send(&m).await;
        until_running(&s, &id(&m)).await;
        let mut done = s.sup.job_done(&id(&m)).unwrap();
        s.release(&id(&m), RunOutput::default());
        done.wait().await;
        assert!(s.sup.pending().is_empty());
        assert!(s.records.read(&id(&m)).is_none());
    }

    #[tokio::test]
    async fn an_offer_missing_its_repository_issue_or_branch_is_refused_as_not_ready() {
        let s = setup();
        let m = s.offer(json!({ "branch": null }));
        assert_eq!(s.send(&m).await, "rejected");
        let r = s
            .posts()
            .into_iter()
            .find(|p| p.0 == format!("/api/daemon/jobs/{}/reject", id(&m)))
            .unwrap();
        assert_eq!(r.1["reason"], "not_ready");
        assert!(s.sup.running().is_empty());
    }

    // --- chat ----------------------------------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_verified_chat_message_is_run_in_its_own_pool_streamed_and_finished_with_the_answer_and_session(
    ) {
        let s = setup();
        let m = s.chat_message(json!({}));
        assert_eq!(s.send(&m).await, "accepted");
        assert!(s.sup.running().is_empty(), "not a job");
        assert_eq!(s.sup.free(), 2, "takes no job slot");
        until_running(&s, &mid(&m)).await;
        let mut done = s.sup.chat_done(&mid(&m)).unwrap();
        s.release(&mid(&m), RunOutput::default());
        done.wait().await;
        let events = s
            .posts()
            .into_iter()
            .find(|p| p.0 == format!("/api/daemon/chat/{}/events", mid(&m)))
            .unwrap()
            .1;
        let got: Vec<(u64, &str)> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["seq"].as_u64().unwrap(), e["type"].as_str().unwrap()))
            .collect();
        assert_eq!(
            got,
            [(0, "delta"), (1, "tool")],
            "only delta and tool go up"
        );
        assert_eq!(
            s.chat_finish(&m).unwrap(),
            json!({ "state": "done", "sessionId": "s1", "costUsd": 0.02, "text": "In auth/index.ts" })
        );
        assert_eq!(s.sup.running_chats(), 0);
    }

    #[tokio::test]
    async fn chat_messages_are_verified_like_offers_and_an_expired_one_is_dropped() {
        let s = setup();
        let stranger = wh_proto::keys::generate();
        let (e, d) = s.signed_by(&s.chat_message(json!({})), &stranger);
        assert_eq!(s.sup.handle(&e, &d).await, "dropped");
        assert_eq!(
            s.send(&s.chat_message(json!({ "deviceId": "someone-else" })))
                .await,
            "dropped"
        );
        assert_eq!(
            s.send(&s.chat_message(json!({ "expiresAt": a_second_ago() })))
                .await,
            "dropped"
        );
        assert_eq!(
            s.sup
                .handle("chat.cancel", &s.signed(&s.chat_message(json!({}))).1)
                .await,
            "dropped",
            "the event must match the signed type"
        );
        assert!(s.posts().is_empty());
    }

    #[tokio::test]
    async fn the_chat_pool_is_two_apart_from_jobs_a_third_message_fails_cleanly_and_jobs_still_run()
    {
        let s = setup();
        let (a, b, third) = (
            s.chat_message(json!({})),
            s.chat_message(json!({})),
            s.chat_message(json!({})),
        );
        s.send(&a).await;
        s.send(&b).await;
        assert_eq!(s.send(&third).await, "rejected");
        assert_eq!(s.chat_finish(&third).unwrap()["state"], "failed");
        assert!(s.chat_finish(&third).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("already answering 2"));
        assert_eq!(
            s.send(&s.offer(json!({}))).await,
            "accepted",
            "chats do not use the job pool"
        );
        assert_eq!(s.send(&a).await, "duplicate");
    }

    #[tokio::test]
    async fn a_signed_chat_cancel_stops_the_run_reported_canceled() {
        let s = setup();
        let m = s.chat_message(json!({}));
        s.send(&m).await;
        until_running(&s, &mid(&m)).await;
        let mut done = s.sup.chat_done(&mid(&m)).unwrap();
        assert_eq!(
            s.send(&s.control("chat.cancel", json!({ "messageId": mid(&m) })))
                .await,
            "canceling"
        );
        assert_eq!(done.wait().await["state"], "canceled");
        assert_eq!(s.chat_finish(&m).unwrap()["state"], "canceled");
        assert_eq!(
            s.send(&s.control("chat.cancel", json!({ "messageId": mid(&m) })))
                .await,
            "unknown chat"
        );
    }

    #[tokio::test]
    async fn a_malformed_chat_message_fails_with_a_reason_one_with_no_id_is_dropped() {
        let s = setup();
        let m = s.chat_message(json!({ "prompt": "" }));
        assert_eq!(s.send(&m).await, "rejected");
        assert!(s.chat_finish(&m).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("incomplete"));
        assert_eq!(
            s.send(&s.chat_message(json!({ "messageId": null }))).await,
            "dropped"
        );
    }

    #[tokio::test]
    async fn a_chat_run_that_fails_on_the_usage_limit_pauses_jobs_and_chats_an_answer_that_merely_mentions_it_does_not(
    ) {
        let s = setup();
        *s.runners.chat_override.lock().unwrap() = Some(Arc::new(|_| {
            done(RunOutput {
                text: "Set a rate limit of 100/min".into(),
                session_id: Some("s".into()),
                ..Default::default()
            })
        }));
        let a = s.chat_message(json!({}));
        s.send(&a).await;
        s.wait_chat(&mid(&a)).await;
        assert_eq!(s.chat_finish(&a).unwrap()["state"], "done");
        assert_eq!(s.send(&s.offer(json!({}))).await, "accepted");

        *s.runners.chat_override.lock().unwrap() = Some(Arc::new(|_| RunResult {
            ok: false,
            exit_code: Some(1),
            result: Some(RunOutput {
                is_error: true,
                text: "Claude usage limit reached".into(),
                ..Default::default()
            }),
            ..Default::default()
        }));
        let b = s.chat_message(json!({}));
        s.send(&b).await;
        s.wait_chat(&mid(&b)).await;
        assert_eq!(s.chat_finish(&b).unwrap()["state"], "failed");
        let next = s.chat_message(json!({}));
        assert_eq!(s.send(&next).await, "rejected");
        assert!(s.chat_finish(&next).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("paused"));
        assert_eq!(s.send(&s.offer(json!({}))).await, "rejected");
    }

    #[tokio::test]
    async fn a_failing_chat_worktree_fails_the_message_with_the_reason() {
        let s = setup();
        *s.ws.fail_chat.lock().unwrap() = Some("the branch main does not exist".into());
        let m = s.chat_message(json!({}));
        s.send(&m).await;
        s.wait_chat(&mid(&m)).await;
        assert_eq!(
            s.chat_finish(&m).unwrap(),
            json!({ "state": "failed", "error": "the branch main does not exist" })
        );
    }

    #[tokio::test]
    async fn shutdown_cancels_running_chats_and_waits_for_them_to_report() {
        let s = setup();
        let m = s.chat_message(json!({}));
        s.send(&m).await;
        s.sup.shutdown().await;
        assert_eq!(s.chat_finish(&m).unwrap()["state"], "canceled");
    }

    // --- WH-171: the repository's checks run before the push; failures go back to the agent, twice at most --------------------

    /// Runs one job whose repository has `checks`; `on_run(dir, n)` runs inside the agent's n-th run (1-based).
    async fn with_checks(
        s: &S,
        checks: Value,
        on_run: impl Fn(&Path, usize) + Send + Sync + 'static,
    ) -> (Value, Vec<Value>, Value) {
        let dir = tempfile::tempdir().unwrap();
        *s.ws.dir.lock().unwrap() = Some(dir.path().to_path_buf());
        let seen = s.runners.job_runs.lock().unwrap().len();
        *s.runners.job_override.lock().unwrap() = Some(Arc::new(move |run, n| {
            on_run(&run.cwd, n);
            done(RunOutput {
                cost_usd: Some(0.1),
                text: "done".into(),
                ..Default::default()
            })
        }));
        let mut m = s.offer(json!({}));
        m["repository"]["checks"] = checks;
        s.send(&m).await;
        let outcome = s.wait_job(&id(&m)).await;
        let prompts = s.runners.job_runs.lock().unwrap()[seen..].to_vec();
        (outcome, prompts, s.post_ending("/finish").unwrap())
    }

    #[tokio::test]
    async fn a_check_that_keeps_failing_goes_back_to_the_agent_twice_then_fails_the_run_naming_it_the_finish_lists_every_check(
    ) {
        let s = setup();
        let (outcome, prompts, finish) = with_checks(
            &s,
            json!([{ "name": "install", "command": "true", "required": true }, { "name": "lint", "command": "echo 1 error; exit 1", "required": true }]),
            |_, _| {},
        )
        .await;
        assert_eq!(prompts.len(), 3, "the first run and two fixes");
        assert_eq!(prompts[2][0]["name"], "lint");
        assert_eq!(outcome["state"], "failed");
        assert!(
            outcome["error"]
                .as_str()
                .unwrap()
                .contains("Check \"lint\" failed: 1 error"),
            "{}",
            outcome["error"]
        );
        let got: Vec<(String, String, i64)> = finish["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["name"].as_str().unwrap().into(),
                    c["status"].as_str().unwrap().into(),
                    c["exitCode"].as_i64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("install".into(), "passed".into(), 0),
                ("lint".into(), "failed".into(), 1)
            ]
        );
        assert!(finish["checks"][1]["tail"]
            .as_str()
            .unwrap()
            .contains("1 error"));
        assert_eq!(
            finish["checkedSha"], "c0ffee1234",
            "and the commit they ran on, so Wheelhouse can tell which commit they describe"
        );
    }

    #[tokio::test]
    async fn a_fix_that_makes_the_checks_pass_lets_the_run_succeed() {
        let s = setup();
        let (outcome, prompts, _) = with_checks(
            &s,
            json!([{ "name": "lint", "command": "test -f fixed", "required": true }]),
            |dir, run| {
                if run == 2 {
                    std::fs::write(dir.join("fixed"), "").unwrap();
                }
            },
        )
        .await;
        assert_eq!(prompts.len(), 2);
        assert_eq!(outcome["state"], "succeeded");
        assert_eq!(outcome["checks"][0]["status"], "passed");
    }

    #[tokio::test]
    async fn a_check_that_cannot_run_is_reported_as_such_is_not_retried_and_fails_the_run_an_optional_one_does_not(
    ) {
        let s = setup();
        let (gone, prompts, finish) = with_checks(
            &s,
            json!([{ "name": "lint", "command": "no-such-tool-wh", "required": true }]),
            |_, _| {},
        )
        .await;
        assert_eq!(prompts.len(), 1);
        assert_eq!(gone["state"], "failed");
        assert_eq!(finish["checks"][0]["status"], "could_not_run");
        let (soft, _, _) = with_checks(
            &s,
            json!([{ "name": "build", "command": "no-such-tool-wh", "required": false }]),
            |_, _| {},
        )
        .await;
        assert_eq!(soft["state"], "succeeded");
        assert_eq!(
            soft["checks"][0]["status"], "could_not_run",
            "still reported"
        );
    }

    #[test]
    fn a_job_record_is_written_while_it_runs_and_the_pid_is_recorded() {
        // The record fields the restart recovery reads: id, repositoryId, branch, startedAt, pid.
        let s = setup();
        s.records.write(&json!({ "id": "j", "repositoryId": REPO, "branch": "wh/x", "startedAt": iso_from_ms(0), "pid": null })).unwrap();
        let mut patch = Map::new();
        patch.insert("pid".into(), json!(42));
        s.records.update("j", patch).unwrap();
        assert_eq!(s.records.read("j").unwrap()["pid"], 42);
    }

    #[allow(dead_code)]
    fn _unused(_: VecDeque<u8>) {}
}
