//! The running daemon: connect, say hello, keep a heartbeat, read the stream and hand its messages to the supervisor, reconnect with
//! backoff when the line drops. It stops for good when Wheelhouse no longer accepts its key (the device was unpaired).
//! Port of `daemon/src/daemon.mjs`.

use crate::cancel::Cancel;
use crate::capabilities::capabilities;
use crate::config::{load_config, update_config, Paths, RunnerConfig};
use crate::connection::{ConnError, Connection, Poster};
use crate::environment::{adopt_user_path, find_claude};
use crate::keystore::load_key;
use crate::runners::ClaudeRunners;
use crate::state::JobRecords;
use crate::supervisor::{Supervisor, SupervisorArgs, SupervisorConfig};
use crate::workspace::Workspace;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use wh_proto::keys::{import_private, public_from_raw};

pub const VERSION: &str = "0.1.0";
pub const HEARTBEAT: Duration = Duration::from_secs(30);

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

/// Wait 1s, 2s, 4s... up to a minute, with some jitter so many machines do not reconnect in step.
pub fn backoff(failures: u32) -> Duration {
    let base = (1000u64 << failures.min(6)).min(60_000) as f64;
    Duration::from_millis((base * (0.75 + rand::random::<f64>() * 0.5)) as u64)
}

/// How a connection problem reads in the log.
fn describe(e: &ConnError) -> String {
    match e {
        ConnError::Http {
            status, message, ..
        } => format!("{status} {message}"),
        other => other.to_string(),
    }
}

/// Send a job's finish. Done once it is received, or refused for good (the job is settled some other way); a failure that may pass (no
/// connection, a server error) keeps the finish with the job's record, so it is sent again on the next connect.
pub async fn report_finish<C: Poster>(
    conn: &C,
    records: &JobRecords,
    log: &Log,
    id: &str,
    body: &Value,
) -> bool {
    match conn
        .post(&format!("/api/daemon/jobs/{id}/finish"), body.clone())
        .await
    {
        Ok(_) => {
            let _ = records.remove(id);
            true
        }
        Err(e) => {
            if matches!(e.status(), Some(s) if s < 500 && s != 429) {
                log(format!(
                    "job {id}: Wheelhouse did not take the finish ({e})"
                ));
                let _ = records.remove(id);
                return true;
            }
            let mut patch = Map::new();
            patch.insert("pendingFinish".into(), body.clone());
            let _ = records.update(id, patch);
            log(format!(
                "job {id}: could not report ({e}); will try again when Wheelhouse is reachable"
            ));
            false
        }
    }
}

/// Finishes that have not got through yet, sent now that the line is up.
pub async fn replay_finishes<C: Poster>(conn: &C, records: &JobRecords, log: &Log) {
    for r in records.all() {
        let Some(id) = r["id"].as_str() else { continue };
        if !r["pendingFinish"].is_object() {
            continue;
        }
        if report_finish(conn, records, log, id, &r["pendingFinish"]).await {
            log(format!(
                "job {id}: reported ({})",
                r["pendingFinish"]["state"].as_str().unwrap_or("")
            ));
        }
    }
}

/// A job a previous run of the daemon was doing when it stopped: stop the agent if it is still alive, and report the job failed.
pub async fn recover<C: Poster>(
    conn: &C,
    records: &JobRecords,
    workspace: &Workspace,
    keep_failed_hours: u64,
    log: &Log,
) {
    for r in records.all() {
        let Some(id) = r["id"].as_str() else { continue };
        if r["pendingFinish"].is_object() {
            continue; // it ended; the finish is sent with the others once connected
        }
        if let Some(pid) = r["pid"].as_u64().filter(|p| *p > 0) {
            if stop_group(pid as u32) {
                log(format!(
                    "job {id}: stopped the agent a previous run left behind (pid {pid})"
                ));
            }
        }
        report_finish(
            conn,
            records,
            log,
            id,
            &json!({ "state": "failed", "error": "The daemon restarted while the job ran" }),
        )
        .await;
        log(format!(
            "job {id}: failed after a restart (its worktree is kept for a look)"
        ));
    }
    workspace.prune_old(keep_failed_hours, 24).await;
}

/// SIGTERM to a process group a previous run left behind. False when it is not running any more.
#[cfg(unix)]
fn stop_group(pid: u32) -> bool {
    // SAFETY: kill(2) on a negative pid signals a process group; a stale pid only yields an error, which means "not running".
    unsafe { libc::kill(-(pid as i32), libc::SIGTERM) == 0 }
}

#[cfg(not(unix))]
fn stop_group(_pid: u32) -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// Wheelhouse no longer accepts this device's key.
    pub unpaired: bool,
}

pub struct DaemonOptions {
    pub paths: Paths,
    /// Raise to stop the daemon: running work is cancelled and reported first.
    pub stop: Cancel,
    pub log: Log,
    /// How often the heartbeat goes out (tests make it short).
    pub heartbeat: Duration,
    /// Skip widening PATH to the user's login shell (tests keep their own).
    pub adopt_path: bool,
    /// The wait before the first retry of a finish that did not get through (1s by default; tests make it short).
    pub finish_retry: Option<Duration>,
}

impl DaemonOptions {
    pub fn new(paths: Paths, stop: Cancel, log: Log) -> Self {
        DaemonOptions {
            paths,
            stop,
            log,
            heartbeat: HEARTBEAT,
            adopt_path: true,
            finish_retry: None,
        }
    }
}

fn text(config: &Map<String, Value>, key: &str) -> Option<String> {
    config
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

pub async fn run_daemon(opts: DaemonOptions) -> Result<Exit, String> {
    let DaemonOptions {
        paths: p,
        stop,
        log,
        heartbeat,
        adopt_path,
        finish_retry,
    } = opts;
    let config = load_config(&p);
    let (Some(device_id), Some(server), Some(server_key)) = (
        text(&config, "deviceId"),
        text(&config, "server"),
        text(&config, "serverPublicKey"),
    ) else {
        return Err(
            "This machine is not paired. Run: wheelhouse-daemon pair --server <url>".into(),
        );
    };
    let keystore = text(&config, "keystore").unwrap_or_else(|| "file".into());
    let Some(secret) = load_key(&keystore, &p.home) else {
        return Err(format!("The device key is missing from the {keystore}. Pair again: wheelhouse-daemon pair --server {server}"));
    };
    let key =
        import_private(&secret).map_err(|e| format!("the device key is not readable: {e}"))?;
    let server_key = public_from_raw(&server_key)
        .map_err(|e| format!("the server's public key is not readable: {e}"))?;

    // Under launchd the PATH is bare: take the user's own, then find claude on it.
    let search_path = if adopt_path {
        adopt_user_path()
    } else {
        std::env::var("PATH").unwrap_or_default()
    };
    let configured_claude = text(&config, "claudePath").unwrap_or_else(|| "claude".into());
    let claude_path = find_claude(&configured_claude, &search_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| configured_claude.clone());
    let home = std::env::var("HOME").unwrap_or_default();

    let runner = RunnerConfig::from_json(&config["runner"]);
    let setup: HashMap<String, String> = config["repositories"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(id, r)| {
                    Some((
                        id.clone(),
                        r["setup"].as_str().filter(|s| !s.is_empty())?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let conn = Arc::new(Connection::new(&server, &device_id, key));
    let workspace = Arc::new(Workspace::new(
        p.clone(),
        text(&config, "gitProtocol").unwrap_or_else(|| "https".into()),
    ));
    let records = Arc::new(JobRecords::new(p.jobs.clone()));
    let supervisor = Supervisor::new(SupervisorArgs {
        conn: conn.clone(),
        device_id: device_id.clone(),
        server_key,
        pool_size: config["poolSize"].as_u64().unwrap_or(3) as usize,
        workspace: workspace.clone(),
        records: records.clone(),
        runners: Arc::new(ClaudeRunners {
            runner: runner.clone(),
            server: server.clone(),
            claude_path: claude_path.clone(),
            home: home.clone(),
        }),
        config: SupervisorConfig {
            timeout_minutes: runner.timeout_minutes,
            setup,
            home,
        },
        log: log.clone(),
        finish_retry,
    });
    {
        let p = p.clone();
        supervisor.on_settings(move |pool, chat_model| {
            let mut patch = Map::new();
            patch.insert("poolSize".into(), json!(pool));
            patch.insert(
                "chatModel".into(),
                chat_model.map(Value::from).unwrap_or(Value::Null),
            );
            let _ = update_config(patch, &p);
        });
    }

    log(format!(
        "wheelhouse-daemon {VERSION}: {} → {server}",
        text(&config, "name").unwrap_or_else(|| "this machine".into())
    ));
    log(format!("claude: {claude_path}"));

    let hello = || async {
        let caps = capabilities(&claude_path, &workspace.held()).await;
        if caps["claude"]["installed"] != json!(true) {
            log(format!("claude was not found (looked for \"{configured_claude}\" on {search_path}): jobs will fail until it is installed, or set \"claudePath\" in config.json"));
        } else if caps["claude"]["signedIn"] != json!(true) {
            log("claude is installed but not signed in: run `claude` once and sign in".into());
        }
        let jobs: Vec<Value> = supervisor
            .running()
            .into_iter()
            .chain(supervisor.pending())
            .map(|id| json!({ "id": id, "state": "running" }))
            .collect();
        let r = conn
            .post(
                "/api/daemon/hello",
                json!({ "version": VERSION, "capabilities": caps, "jobs": jobs }),
            )
            .await?;
        if let Some(n) = r["device"]["poolSize"].as_u64().filter(|n| *n > 0) {
            if n as usize != supervisor.pool_size() {
                supervisor.settings(&json!({ "type": "settings.update", "poolSize": n, "chatModel": r["device"]["chatModel"] }));
            }
        }
        Ok::<(), ConnError>(())
    };

    // Only a change is logged (it stopped working, it works again), so the log shows when Wheelhouse could not hear this machine.
    // The heartbeat can see the device was unpaired before the main loop does. It says so here before it stops the daemon, so the exit
    // still reads as "unpaired" (a clean exit: the service stays stopped) and not as a plain stop (the service would start it again).
    let unpaired_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let beat = {
        let (conn, supervisor, stop, log, seen) = (
            conn.clone(),
            supervisor.clone(),
            stop.clone(),
            log.clone(),
            unpaired_seen.clone(),
        );
        tokio::spawn(async move {
            let mut failing = false;
            let mut tick =
                tokio::time::interval_at(tokio::time::Instant::now() + heartbeat, heartbeat);
            loop {
                tick.tick().await;
                let running: Vec<String> = supervisor
                    .running()
                    .into_iter()
                    .chain(supervisor.pending())
                    .collect();
                match conn.post("/api/daemon/heartbeat", json!({ "running": running, "free": supervisor.free(), "onBattery": false })).await {
                    Ok(_) => {
                        if failing {
                            log("heartbeat: reaching Wheelhouse again".into());
                        }
                        failing = false;
                    }
                    Err(e) => {
                        if e == ConnError::Unpaired {
                            seen.store(true, std::sync::atomic::Ordering::SeqCst);
                            stop.cancel();
                        }
                        if !failing {
                            log(format!("heartbeat: cannot reach Wheelhouse ({}); it shows this machine offline until this works", describe(&e)));
                        }
                        failing = true;
                    }
                }
            }
        })
    };

    let mut unpaired = false;
    recover(
        &*conn,
        &records,
        &workspace,
        config["keepFailedHours"].as_u64().unwrap_or(24),
        &log,
    )
    .await;
    let mut failures: u32 = 0;
    let mut last_event_id: Option<String> = config
        .get("lastEventId")
        .and_then(Value::as_str)
        .map(String::from);
    while !stop.is_cancelled() {
        let round: Result<(), ConnError> = async {
            hello().await?;
            log("connected".into());
            replay_finishes(&*conn, &records, &log).await;
            let mut stream = conn.stream(last_event_id.as_deref()).await?;
            loop {
                let next = tokio::select! {
                    m = stream.next() => m?,
                    _ = stop.cancelled() => return Ok(()),
                };
                let Some(m) = next else { break };
                failures = 0;
                supervisor.handle(&m.event, &m.data).await;
                if let Some(id) = m.id {
                    let mut patch = Map::new();
                    patch.insert("lastEventId".into(), json!(id));
                    let _ = update_config(patch, &p);
                    last_event_id = Some(id);
                }
            }
            if !stop.is_cancelled() {
                log("the stream ended; reconnecting".into());
            }
            Ok(())
        }
        .await;
        match round {
            Ok(()) => {}
            Err(_) if stop.is_cancelled() => break,
            Err(e @ ConnError::Unpaired) => {
                unpaired = true;
                log(e.to_string());
                break;
            }
            Err(e) => {
                failures += 1;
                let wait = backoff(failures);
                log(format!(
                    "connection problem ({}); retrying in {}s",
                    describe(&e),
                    wait.as_secs_f64().round() as u64
                ));
                tokio::select! { _ = tokio::time::sleep(wait) => {}, _ = stop.cancelled() => {} }
            }
        }
    }
    beat.abort();
    supervisor.shutdown().await;
    Ok(Exit {
        unpaired: unpaired || unpaired_seen.load(std::sync::atomic::Ordering::SeqCst),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use wh_proto::sign::sign_value;

    fn quiet_log() -> (Log, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(vec![]));
        let l2 = lines.clone();
        (Arc::new(move |m| l2.lock().unwrap().push(m)), lines)
    }

    #[test]
    fn the_backoff_doubles_up_to_a_minute_with_jitter() {
        for (failures, base) in [
            (0u32, 1000.0),
            (1, 2000.0),
            (3, 8000.0),
            (6, 60_000.0),
            (20, 60_000.0),
        ] {
            for _ in 0..50 {
                let ms = backoff(failures).as_millis() as f64;
                assert!(
                    ms >= base * 0.75 - 1.0 && ms <= base * 1.25 + 1.0,
                    "{failures}: {ms}"
                );
            }
        }
    }

    // --- finishes and recovery, against a stand-in poster ---------------------------------------------------------------------

    #[derive(Default)]
    struct Fake {
        posts: Mutex<Vec<(String, Value)>>,
        fail: Mutex<VecDeque<ConnError>>,
    }

    impl Poster for Fake {
        async fn post(&self, path: &str, body: Value) -> Result<Value, ConnError> {
            if let Some(e) = self.fail.lock().unwrap().pop_front() {
                return Err(e);
            }
            self.posts.lock().unwrap().push((path.to_string(), body));
            Ok(json!({}))
        }
    }

    fn http(status: u16) -> ConnError {
        ConnError::Http {
            status,
            code: "x".into(),
            message: "nope".into(),
        }
    }

    fn records() -> (tempfile::TempDir, JobRecords) {
        let d = tempfile::tempdir().unwrap();
        let r = JobRecords::new(d.path().join("jobs"));
        (d, r)
    }

    #[tokio::test]
    async fn a_finish_is_done_when_received_or_refused_for_good_and_kept_when_it_may_pass() {
        let (_d, rec) = records();
        let (log, lines) = quiet_log();
        let conn = Fake::default();
        let body = json!({ "state": "succeeded" });

        rec.write(&json!({ "id": "a" })).unwrap();
        assert!(report_finish(&conn, &rec, &log, "a", &body).await);
        assert!(rec.read("a").is_none());
        assert_eq!(conn.posts.lock().unwrap()[0].0, "/api/daemon/jobs/a/finish");

        rec.write(&json!({ "id": "b" })).unwrap();
        conn.fail.lock().unwrap().push_back(http(409));
        assert!(
            report_finish(&conn, &rec, &log, "b", &body).await,
            "settled some other way"
        );
        assert!(rec.read("b").is_none());

        for e in [http(503), http(429), ConnError::Transport("down".into())] {
            rec.write(&json!({ "id": "c" })).unwrap();
            conn.fail.lock().unwrap().push_back(e);
            assert!(!report_finish(&conn, &rec, &log, "c", &body).await);
            assert_eq!(rec.read("c").unwrap()["pendingFinish"], body);
        }
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("will try again when Wheelhouse is reachable")));
    }

    #[tokio::test]
    async fn stored_finishes_are_sent_again_once_connected_and_removed() {
        let (_d, rec) = records();
        let (log, _) = quiet_log();
        let conn = Fake::default();
        rec.write(&json!({ "id": "a", "pendingFinish": { "state": "succeeded" } }))
            .unwrap();
        rec.write(&json!({ "id": "b", "pid": null })).unwrap(); // still running: not replayed
        replay_finishes(&conn, &rec, &log).await;
        let posts = conn.posts.lock().unwrap().clone();
        assert_eq!(posts.len(), 1);
        assert_eq!(
            (posts[0].0.as_str(), &posts[0].1),
            (
                "/api/daemon/jobs/a/finish",
                &json!({ "state": "succeeded" })
            )
        );
        assert!(rec.read("a").is_none() && rec.read("b").is_some());
    }

    #[tokio::test]
    async fn a_job_a_previous_run_left_is_stopped_and_reported_failed_after_a_restart() {
        let (d, rec) = records();
        let (log, lines) = quiet_log();
        let conn = Fake::default();
        let ws = Workspace::new(Paths::new(d.path().join("home")), "https");
        // A real agent left behind: a process in its own group.
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("300");
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut left = cmd.spawn().unwrap();
        rec.write(&json!({ "id": "lost", "pid": left.id() }))
            .unwrap();
        rec.write(&json!({ "id": "ended", "pendingFinish": { "state": "succeeded" } }))
            .unwrap();
        recover(&conn, &rec, &ws, 24, &log).await;
        let posts = conn.posts.lock().unwrap().clone();
        assert_eq!(
            posts.len(),
            1,
            "only the lost job is reported now; the ended one waits for the connection"
        );
        assert_eq!(posts[0].0, "/api/daemon/jobs/lost/finish");
        assert_eq!(
            posts[0].1,
            json!({ "state": "failed", "error": "The daemon restarted while the job ran" })
        );
        assert!(rec.read("lost").is_none() && rec.read("ended").is_some());
        let status = tokio::task::spawn_blocking(move || left.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success(), "the leftover agent was stopped");
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("stopped the agent a previous run left behind")));
    }

    // --- the whole daemon against a mock Wheelhouse ---------------------------------------------------------------------------

    /// A mock server: `handler(request_text, connection_number) -> Option<raw response>`; `HOLD:` keeps the connection open and silent
    /// after sending. It records each request's head.
    async fn serve(
        handler: impl Fn(&str, usize) -> String + Send + Sync + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(vec![]));
        let (handler, seen2) = (Arc::new(handler), seen.clone());
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = l.accept().await.unwrap();
                let (handler, seen, n) = (handler.clone(), seen2.clone(), n.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let got = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..got]).into_owned();
                    seen.lock().unwrap().push(req.clone());
                    let i = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let r = handler(&req, i);
                    let hold = r.starts_with("HOLD:");
                    sock.write_all(r.strip_prefix("HOLD:").unwrap_or(&r).as_bytes())
                        .await
                        .ok();
                    if hold {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                });
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn json_response(status: u16, body: &str) -> String {
        format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
    }

    fn token() -> String {
        json_response(200, r#"{"accessToken":"whd_x","expiresIn":900}"#)
    }

    fn paired(server: &str, server_key: &ed25519_dalek::SigningKey) -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path().join("daemon"));
        let key = wh_proto::keys::generate();
        crate::keystore::store_key("file", &p.home, &wh_proto::keys::export_private(&key)).unwrap();
        let mut patch = Map::new();
        for (k, v) in [
            ("server", json!(server)),
            ("deviceId", json!("11111111-1111-1111-1111-111111111111")),
            (
                "serverPublicKey",
                json!(wh_proto::keys::raw_public(&server_key.verifying_key())),
            ),
            ("keystore", json!("file")),
            ("claudePath", json!("/nonexistent/claude")),
        ] {
            patch.insert(k.into(), v);
        }
        update_config(patch, &p).unwrap();
        (dir, p)
    }

    async fn run(p: &Paths, stop: Cancel, log: Log) -> Result<Exit, String> {
        let mut o = DaemonOptions::new(p.clone(), stop, log);
        o.heartbeat = Duration::from_millis(100);
        o.adopt_path = false;
        run_daemon(o).await
    }

    #[tokio::test]
    async fn an_unpaired_machine_says_how_to_pair() {
        let dir = tempfile::tempdir().unwrap();
        let (log, _) = quiet_log();
        let e = run(&Paths::new(dir.path().join("none")), Cancel::new(), log)
            .await
            .unwrap_err();
        assert!(e.contains("not paired") && e.contains("wheelhouse-daemon pair --server"));
    }

    #[tokio::test]
    async fn a_missing_device_key_says_to_pair_again() {
        let server_key = wh_proto::keys::generate();
        let (_d, p) = paired("http://127.0.0.1:1", &server_key);
        crate::keystore::delete_key("file", &p.home);
        let (log, _) = quiet_log();
        let e = run(&p, Cancel::new(), log).await.unwrap_err();
        assert!(
            e.contains("device key is missing") && e.contains("Pair again"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn the_daemon_says_hello_follows_the_stream_saves_where_it_got_to_reconnects_from_there_and_stops_when_asked(
    ) {
        let server_key = wh_proto::keys::generate();
        let settings = json!({ "type": "settings.update", "deviceId": "11111111-1111-1111-1111-111111111111", "poolSize": 4, "chatModel": "opus" });
        let signed =
            json!({ "message": settings, "signature": sign_value(&server_key, &settings) })
                .to_string();
        let sse = format!("id: 7\nevent: settings.update\ndata: {signed}\n\n");
        let hellos = std::sync::atomic::AtomicUsize::new(0);
        let (server, seen) = serve(move |req, _| {
            if req.starts_with("POST /api/daemon/token") {
                token()
            } else if req.starts_with("POST /api/daemon/hello") {
                // The first answer changes the pool size (the daemon saves it); later ones agree with what the stream set.
                if hellos.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    json_response(200, r#"{"device":{"poolSize":5,"chatModel":null}}"#)
                } else {
                    json_response(200, r#"{"device":{"poolSize":4,"chatModel":"opus"}}"#)
                }
            } else if req.starts_with("GET /api/daemon/stream") {
                if req.to_lowercase().contains("last-event-id: 7") {
                    "HOLD:HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n: keep-alive\n\n".to_string()
                } else {
                    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{sse}")
                }
            } else {
                json_response(200, "{}")
            }
        })
        .await;
        let (_d, p) = paired(&server, &server_key);
        let (log, lines) = quiet_log();
        let stop = Cancel::new();
        let (p2, s2) = (p.clone(), stop.clone());
        let daemon = tokio::spawn(async move { run(&p2, s2, log).await });

        // Wait until the second stream (resumed after id 7) has been opened, and a heartbeat has gone out.
        for _ in 0..200 {
            let ready = {
                let s = seen.lock().unwrap();
                let resumed = s
                    .iter()
                    .any(|r| r.to_lowercase().contains("last-event-id: 7"));
                let beat = s
                    .iter()
                    .any(|r| r.starts_with("POST /api/daemon/heartbeat"));
                resumed && beat
            };
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        stop.cancel();
        let exit = tokio::time::timeout(Duration::from_secs(10), daemon)
            .await
            .expect("stops")
            .unwrap()
            .unwrap();
        assert_eq!(exit, Exit { unpaired: false });

        let s = seen.lock().unwrap().clone();
        let hello = s
            .iter()
            .find(|r| r.starts_with("POST /api/daemon/hello"))
            .expect("hello");
        assert!(
            hello.contains(&format!("\"version\":\"{VERSION}\""))
                && hello.contains("\"capabilities\"")
                && hello.contains("\"jobs\":[]"),
            "{hello}"
        );
        assert!(hello.to_lowercase().contains("authorization: bearer whd_x"));
        let beat = s
            .iter()
            .find(|r| r.starts_with("POST /api/daemon/heartbeat"))
            .expect("heartbeat");
        assert!(
            beat.contains("\"free\":") && beat.contains("\"onBattery\":false"),
            "{beat}"
        );
        assert!(
            s.iter()
                .filter(|r| r.starts_with("POST /api/daemon/hello"))
                .count()
                >= 2,
            "a hello on every connect"
        );
        let cfg = load_config(&p);
        assert_eq!(cfg["lastEventId"], "7");
        assert_eq!(
            (cfg["poolSize"].clone(), cfg["chatModel"].clone()),
            (json!(4), json!("opus")),
            "the stream's settings are saved"
        );
        let lines = lines.lock().unwrap();
        assert!(lines.iter().any(|l| l == "connected"));
        assert!(lines
            .iter()
            .any(|l| l.contains("the stream ended; reconnecting")));
        assert!(lines.iter().any(|l| l.contains("claude was not found")));
    }

    #[tokio::test]
    async fn a_device_wheelhouse_no_longer_knows_stops_the_daemon_for_good() {
        let server_key = wh_proto::keys::generate();
        let (server, _) = serve(|_, _| json_response(401, "{}")).await;
        let (_d, p) = paired(&server, &server_key);
        let (log, lines) = quiet_log();
        let exit = tokio::time::timeout(Duration::from_secs(10), run(&p, Cancel::new(), log))
            .await
            .expect("stops")
            .unwrap();
        assert_eq!(exit, Exit { unpaired: true });
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("no longer accepts this device's key")));
    }

    #[tokio::test]
    async fn unpaired_is_reported_even_when_the_heartbeat_notices_before_the_main_loop() {
        let server_key = wh_proto::keys::generate();
        let (server, _) = serve(|_, _| json_response(401, "{}")).await;
        let (_d, p) = paired(&server, &server_key);
        // A heartbeat every millisecond races the first hello to the 401. Either way the exit must say "unpaired", so a service
        // manager sees a clean exit and does not start the daemon again.
        for round in 0..25 {
            let (log, _) = quiet_log();
            let mut o = DaemonOptions::new(p.clone(), Cancel::new(), log);
            o.heartbeat = Duration::from_millis(1);
            o.adopt_path = false;
            let exit = tokio::time::timeout(Duration::from_secs(10), run_daemon(o))
                .await
                .expect("stops")
                .unwrap();
            assert_eq!(exit, Exit { unpaired: true }, "round {round}");
        }
    }

    #[tokio::test]
    async fn an_unreachable_server_is_retried_with_a_logged_backoff_until_stopped() {
        let server_key = wh_proto::keys::generate();
        let (_d, p) = paired("http://127.0.0.1:1", &server_key);
        let (log, lines) = quiet_log();
        let stop = Cancel::new();
        let s2 = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            s2.cancel();
        });
        let exit = tokio::time::timeout(Duration::from_secs(10), run(&p, stop, log))
            .await
            .expect("stops")
            .unwrap();
        assert!(!exit.unpaired);
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("connection problem") && l.contains("retrying in")));
    }
}
