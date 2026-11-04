//! The daemon's line to Wheelhouse: it always dials out. Messages come down one SSE stream; everything the daemon reports goes up as
//! plain POSTs. Requests carry a 15-minute token bought by signing an assertion with the device key, renewed before it runs out.
//! Port of `daemon/src/connection.mjs` (the SSE parser itself lives in `wh-proto`).

use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use wh_proto::{make_assertion, SseMessage, SseParser};

/// A request that gets no answer in this long is given up on (a connection that died quietly, after the laptop slept, never answers).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The server sends a keep-alive every 25 seconds; a stream silent for this long is dead, and is dropped so the daemon reconnects.
pub const STREAM_IDLE: Duration = Duration::from_secs(75);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnError {
    /// The server answered with an error status.
    Http {
        status: u16,
        code: String,
        message: String,
    },
    /// The device has been unpaired (or its key is not the one Wheelhouse knows): nothing works until it is paired again.
    Unpaired,
    /// A request got no answer in time.
    Timeout,
    /// The stream sent nothing, not even a keep-alive, for this many seconds.
    Quiet(u64),
    /// No connection, a reset, a bad body.
    Transport(String),
}

impl ConnError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ConnError::Http { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl std::fmt::Display for ConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnError::Http { message, .. } => write!(f, "{message}"),
            ConnError::Unpaired => write!(
                f,
                "Wheelhouse no longer accepts this device's key; it was probably unpaired"
            ),
            ConnError::Timeout => write!(f, "the request timed out"),
            ConnError::Quiet(s) => write!(f, "the stream went quiet for {s}s"),
            ConnError::Transport(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ConnError {}

fn transport(e: reqwest::Error) -> ConnError {
    if e.is_timeout() {
        ConnError::Timeout
    } else {
        ConnError::Transport(e.to_string())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Where a job reporter sends its events: the real connection, or a stand-in in tests.
pub trait Poster: Send + Sync {
    fn post(
        &self,
        path: &str,
        body: Value,
    ) -> impl Future<Output = Result<Value, ConnError>> + Send;
}

pub struct Connection {
    server: String,
    device_id: String,
    key: SigningKey,
    client: reqwest::Client,
    request_timeout: Duration,
    stream_idle: Duration,
    now: fn() -> u64,
    cached: Mutex<Option<(String, u64)>>, // token, expires at (ms)
}

impl Connection {
    pub fn new(server: &str, device_id: &str, key: SigningKey) -> Self {
        Connection {
            server: server.trim_end_matches('/').to_string(),
            device_id: device_id.to_string(),
            key,
            client: reqwest::Client::new(),
            request_timeout: REQUEST_TIMEOUT,
            stream_idle: STREAM_IDLE,
            now: now_ms,
            cached: Mutex::new(None),
        }
    }

    pub fn with_timeouts(mut self, request: Duration, stream_idle: Duration) -> Self {
        self.request_timeout = request;
        self.stream_idle = stream_idle;
        self
    }

    pub fn with_clock(mut self, now: fn() -> u64) -> Self {
        self.now = now;
        self
    }

    pub async fn token(&self) -> Result<String, ConnError> {
        let mut cached = self.cached.lock().await;
        if let Some((t, exp)) = cached.as_ref() {
            if exp.saturating_sub((self.now)()) > 60_000 {
                return Ok(t.clone());
            }
        }
        let res = self
            .client
            .post(format!("{}/api/daemon/token", self.server))
            .header("accept", "application/json")
            .timeout(self.request_timeout)
            .json(&json!({ "assertion": make_assertion(&self.device_id, &self.key, (self.now)()) }))
            .send()
            .await
            .map_err(transport)?;
        let status = res.status();
        let body: Option<Value> = res.json().await.ok();
        if status.as_u16() == 401 {
            return Err(ConnError::Unpaired);
        }
        if !status.is_success() {
            return Err(http_error(
                status.as_u16(),
                body.as_ref(),
                "token request failed",
            ));
        }
        let body = body.unwrap_or(Value::Null);
        let (Some(token), Some(expires_in)) =
            (body["accessToken"].as_str(), body["expiresIn"].as_f64())
        else {
            return Err(ConnError::Http {
                status: status.as_u16(),
                code: "bad_response".into(),
                message: "the token response had no accessToken".into(),
            });
        };
        *cached = Some((
            token.to_string(),
            (self.now)() + (expires_in * 1000.0) as u64,
        ));
        Ok(token.to_string())
    }

    /// A JSON request with the device token. A 401 (the token was dropped server side) buys a fresh token and tries once more.
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ConnError> {
        let mut retried = false;
        loop {
            let mut req = self
                .client
                .request(method.clone(), format!("{}{}", self.server, path))
                .bearer_auth(self.token().await?)
                .header("accept", "application/json")
                .timeout(self.request_timeout);
            if let Some(b) = body {
                req = req.json(b);
            }
            let res = req.send().await.map_err(transport)?;
            let status = res.status();
            let json: Option<Value> = res.json().await.ok();
            if status.as_u16() == 401 && !retried {
                *self.cached.lock().await = None;
                retried = true;
                continue;
            }
            if !status.is_success() {
                return Err(http_error(
                    status.as_u16(),
                    json.as_ref(),
                    &format!("{method} {path} failed"),
                ));
            }
            return Ok(json.unwrap_or(Value::Null));
        }
    }

    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value, ConnError> {
        self.request(reqwest::Method::POST, path, Some(body)).await
    }

    /// Open the stream once. Reconnecting is the caller's job: it knows the last id it handled. Dropping the stream closes it.
    pub async fn stream(&self, last_event_id: Option<&str>) -> Result<EventStream, ConnError> {
        let quiet = || ConnError::Quiet(self.stream_idle.as_secs());
        let token = tokio::time::timeout(self.stream_idle, self.token())
            .await
            .map_err(|_| quiet())??;
        let mut req = self
            .client
            .get(format!("{}/api/daemon/stream", self.server))
            .bearer_auth(token)
            .header("accept", "text/event-stream");
        if let Some(id) = last_event_id {
            req = req.header("last-event-id", id);
        }
        let res = tokio::time::timeout(self.stream_idle, req.send())
            .await
            .map_err(|_| quiet())?
            .map_err(transport)?;
        let status = res.status().as_u16();
        if status == 401 {
            *self.cached.lock().await = None;
            return Err(ConnError::Http {
                status,
                code: "unauthorized".into(),
                message: "the stream refused the token".into(),
            });
        }
        if !res.status().is_success() {
            return Err(ConnError::Http {
                status,
                code: "http_error".into(),
                message: format!("stream failed ({status})"),
            });
        }
        Ok(EventStream {
            res,
            parser: SseParser::new(),
            pending: VecDeque::new(),
            tail: vec![],
            idle: self.stream_idle,
        })
    }
}

impl Poster for Connection {
    async fn post(&self, path: &str, body: Value) -> Result<Value, ConnError> {
        self.post_json(path, &body).await
    }
}

fn http_error(status: u16, body: Option<&Value>, fallback: &str) -> ConnError {
    let err = body.and_then(|b| b.get("error"));
    ConnError::Http {
        status,
        code: err
            .and_then(|e| e["code"].as_str())
            .unwrap_or("http_error")
            .to_string(),
        message: err
            .and_then(|e| e["message"].as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("{fallback} ({status})")),
    }
}

/// The open stream: `next` yields messages until the stream ends (`Ok(None)`), fails, or goes quiet.
pub struct EventStream {
    res: reqwest::Response,
    parser: SseParser,
    pending: VecDeque<SseMessage>,
    tail: Vec<u8>, // an unfinished UTF-8 sequence from the last chunk
    idle: Duration,
}

impl EventStream {
    pub async fn next(&mut self) -> Result<Option<SseMessage>, ConnError> {
        loop {
            if let Some(m) = self.pending.pop_front() {
                return Ok(Some(m));
            }
            let chunk = match tokio::time::timeout(self.idle, self.res.chunk()).await {
                Err(_) => return Err(ConnError::Quiet(self.idle.as_secs())),
                Ok(Err(e)) => return Err(transport(e)),
                Ok(Ok(None)) => return Ok(None),
                Ok(Ok(Some(c))) => c,
            };
            self.tail.extend_from_slice(&chunk);
            let text = match std::str::from_utf8(&self.tail) {
                Ok(s) => {
                    let s = s.to_string();
                    self.tail.clear();
                    s
                }
                Err(e) if e.error_len().is_none() => {
                    let good = e.valid_up_to();
                    let s = String::from_utf8_lossy(&self.tail[..good]).into_owned();
                    self.tail.drain(..good);
                    s
                }
                Err(_) => {
                    let s = String::from_utf8_lossy(&self.tail).into_owned();
                    self.tail.clear();
                    s
                }
            };
            self.pending.extend(self.parser.push(&text));
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------

/// `Date.prototype.toISOString` for now: `2026-11-03T22:52:55.525Z`.
pub fn iso_now() -> String {
    iso_from_ms(now_ms())
}

pub fn iso_from_ms(ms: u64) -> String {
    let (secs, milli) = (ms / 1000, ms % 1000);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// `Date.parse` for the timestamps Wheelhouse sends (`2026-11-03T22:52:55.525Z`, or with a `+hh:mm` offset): milliseconds since the epoch.
pub fn parse_iso_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> { s.get(from..from + len)?.parse().ok() };
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, m, d, hh, mm, ss) = (
        num(0, 4)?,
        num(5, 2)?,
        num(8, 2)?,
        num(11, 2)?,
        num(14, 2)?,
        num(17, 2)?,
    );
    let mut i = 19;
    let mut milli = 0i64;
    if b.get(i) == Some(&b'.') {
        let digits: String = s[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            return None;
        }
        milli = format!("{:0<3}", &digits[..digits.len().min(3)])
            .parse()
            .ok()?;
        i += 1 + digits.len();
    }
    let offset_min = match s.get(i..)? {
        "Z" | "z" => 0,
        t if t.len() == 6 && (t.starts_with('+') || t.starts_with('-')) && &t[3..4] == ":" => {
            let v = t[1..3].parse::<i64>().ok()? * 60 + t[4..6].parse::<i64>().ok()?;
            if t.starts_with('-') {
                -v
            } else {
                v
            }
        }
        _ => return None,
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 24 || mm > 59 || ss > 59 {
        return None;
    }
    // Days since 1970-01-01 from a civil date (Howard Hinnant's algorithm).
    let y2 = y - i64::from(m <= 2);
    let era = y2.div_euclid(400);
    let yoe = y2.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let ms = ((days * 24 + hh) * 60 + mm - offset_min) * 60_000 + ss * 1000 + milli;
    u64::try_from(ms).ok()
}

#[derive(Debug, Clone)]
pub struct ReporterConfig {
    pub flush: Duration,
    pub batch: usize,
    pub max_queued: usize,
    pub finish_retry: Duration,
}

impl Default for ReporterConfig {
    fn default() -> Self {
        ReporterConfig {
            flush: Duration::from_millis(250),
            batch: 50,
            max_queued: 2000,
            finish_retry: Duration::from_millis(1000),
        }
    }
}

/// What became of the final state: `retry` is true when it did not get through for a reason that may pass (no connection, a server
/// error), so the caller keeps it and sends it again later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinishOutcome {
    pub sent: bool,
    pub retry: bool,
}

struct State {
    seq: u64,
    queue: Vec<Value>,
    timer_armed: bool,
    failures: u32,
}

struct Inner<P> {
    poster: Arc<P>,
    job_id: String,
    path: String, // where this run's events and finish go: a job's, or a chat message's
    cfg: ReporterConfig,
    log: Box<dyn Fn(String) + Send + Sync>,
    state: StdMutex<State>,
    sending: Mutex<()>,
}

/// A job's progress on its way up: events are numbered here (the server stores each number once, so a resent batch is harmless) and
/// sent in batches, every quarter second or every 50 events. A failed send keeps the events and tries again later; past 2,000
/// waiting, the oldest are dropped so a long outage cannot eat memory.
pub struct JobReporter<P: Poster + 'static> {
    inner: Arc<Inner<P>>,
}

impl<P: Poster + 'static> Clone for JobReporter<P> {
    fn clone(&self) -> Self {
        JobReporter {
            inner: self.inner.clone(),
        }
    }
}

type Flush<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

impl<P: Poster + 'static> Inner<P> {
    fn arm(self: &Arc<Self>, delay: Duration) {
        {
            let mut s = self.state.lock().unwrap();
            if s.timer_armed {
                return;
            }
            s.timer_armed = true;
        }
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            me.flush().await;
        });
    }

    fn flush(self: &Arc<Self>) -> Flush<'_> {
        Box::pin(async move {
            let _sending = self.sending.lock().await;
            let batch: Vec<Value> = {
                let mut s = self.state.lock().unwrap();
                s.timer_armed = false;
                s.queue.iter().take(200).cloned().collect()
            };
            if batch.is_empty() {
                return;
            }
            let n = batch.len();
            let result = self
                .poster
                .post(&format!("{}/events", self.path), json!({ "events": batch }))
                .await;
            let more = {
                let mut s = self.state.lock().unwrap();
                match &result {
                    Ok(_) => {
                        let n = n.min(s.queue.len());
                        s.queue.drain(..n);
                        s.failures = 0;
                    }
                    Err(e) => {
                        s.failures = s.failures.saturating_add(1);
                        // A finished job's events are refused for good: stop trying.
                        if matches!(e.status(), Some(409 | 404)) {
                            s.queue.clear();
                        }
                    }
                }
                !s.queue.is_empty()
            };
            if let Err(e) = &result {
                (self.log)(format!(
                    "job {}: could not send progress ({e}); will retry",
                    self.job_id
                ));
            }
            if more {
                let failures = self.state.lock().unwrap().failures;
                let delay = (self.cfg.flush * 2u32.saturating_pow(failures.min(16)))
                    .min(Duration::from_secs(30));
                self.arm(delay);
            }
        })
    }
}

impl<P: Poster + 'static> JobReporter<P> {
    pub fn new(
        poster: Arc<P>,
        job_id: &str,
        path: Option<&str>,
        cfg: ReporterConfig,
        log: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        JobReporter {
            inner: Arc::new(Inner {
                poster,
                job_id: job_id.to_string(),
                path: path
                    .map(String::from)
                    .unwrap_or_else(|| format!("/api/daemon/jobs/{job_id}")),
                cfg,
                log: Box::new(log),
                state: StdMutex::new(State {
                    seq: 0,
                    queue: vec![],
                    timer_armed: false,
                    failures: 0,
                }),
                sending: Mutex::new(()),
            }),
        }
    }

    /// Queue an event. Needs a running tokio runtime (the send happens on a task).
    pub fn event(&self, kind: &str, data: Value) {
        let full = {
            let mut s = self.inner.state.lock().unwrap();
            let seq = s.seq;
            s.seq += 1;
            s.queue
                .push(json!({ "seq": seq, "at": iso_now(), "type": kind, "data": data }));
            let over = s.queue.len().saturating_sub(self.inner.cfg.max_queued);
            s.queue.drain(..over);
            s.queue.len() >= self.inner.cfg.batch
        };
        if full {
            let me = self.inner.clone();
            tokio::spawn(async move { me.flush().await });
        } else {
            self.inner.arm(self.inner.cfg.flush);
        }
    }

    pub async fn flush(&self) {
        self.inner.flush().await;
    }

    /// Events still waiting to go up.
    pub fn queued(&self) -> usize {
        self.inner.state.lock().unwrap().queue.len()
    }

    /// Send what is left (a few tries), then the final state.
    pub async fn finish(&self, result: Value) -> FinishOutcome {
        for _ in 0..5 {
            if self.queued() == 0 {
                break;
            }
            self.flush().await;
        }
        let mut last: Option<ConnError> = None;
        for i in 0..5u32 {
            match self
                .inner
                .poster
                .post(&format!("{}/finish", self.inner.path), result.clone())
                .await
            {
                Ok(_) => {
                    return FinishOutcome {
                        sent: true,
                        retry: false,
                    }
                }
                Err(e) => {
                    if matches!(e.status(), Some(s) if s < 500 && s != 429) {
                        (self.inner.log)(format!(
                            "job {}: could not report the finish ({e})",
                            self.inner.job_id
                        ));
                        return FinishOutcome {
                            sent: false,
                            retry: false,
                        };
                    }
                    last = Some(e);
                    tokio::time::sleep(self.inner.cfg.finish_retry * 2u32.pow(i)).await;
                }
            }
        }
        let why = last.map(|e| e.to_string()).unwrap_or_default();
        (self.inner.log)(format!("job {}: could not report the finish ({why}); keeping it to send when Wheelhouse is reachable", self.inner.job_id));
        FinishOutcome {
            sent: false,
            retry: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A one-route mock server. `handler(request_text, connection_number) -> Option<raw response>`; `None` leaves the connection open
    /// without answering. A response of `HOLD:<bytes>` is sent as given, and the connection is then kept open and silent.
    async fn serve(
        handler: impl Fn(String, usize) -> Option<String> + Send + Sync + 'static,
    ) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let handler = Arc::new(handler);
        let n = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = l.accept().await.unwrap();
                let (handler, n) = (handler.clone(), n.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let got = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..got]).into_owned();
                    let i = n.fetch_add(1, Ordering::SeqCst);
                    match handler(req, i) {
                        Some(r) => {
                            let r = r.strip_prefix("HOLD:").unwrap_or(&r).to_string();
                            sock.write_all(r.as_bytes()).await.ok();
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                        None => tokio::time::sleep(Duration::from_secs(30)).await,
                    }
                });
            }
        });
        format!("http://{addr}")
    }

    fn json_response(status: u16, body: &str) -> String {
        format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
    }

    fn token_ok() -> String {
        json_response(200, r#"{"accessToken":"whd_x","expiresIn":900}"#)
    }

    fn conn(server: &str, request: Duration, idle: Duration) -> Connection {
        Connection::new(server, "d", wh_proto::keys::generate()).with_timeouts(request, idle)
    }

    #[tokio::test]
    async fn a_request_that_is_never_answered_is_given_up_on_instead_of_hanging() {
        let server = serve(|req, _| {
            if req.contains("/api/daemon/token") {
                Some(token_ok())
            } else {
                None
            }
        })
        .await;
        let c = conn(&server, Duration::from_millis(100), STREAM_IDLE);
        let e = c
            .post_json("/api/daemon/heartbeat", &json!({}))
            .await
            .unwrap_err();
        assert_eq!(e, ConnError::Timeout);
    }

    #[tokio::test]
    async fn a_stream_that_goes_quiet_is_dropped_with_a_clear_error_so_the_daemon_reconnects() {
        let server = serve(|req, _| {
            if req.contains("/api/daemon/token") {
                Some(token_ok())
            } else {
                Some("HOLD:HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\nretry: 5000\n\n".into())
            }
        })
        .await;
        let c = conn(&server, REQUEST_TIMEOUT, Duration::from_millis(150));
        let mut s = c.stream(None).await.unwrap();
        let e = s.next().await.unwrap_err();
        assert!(e.to_string().contains("went quiet"), "{e}");
    }

    #[tokio::test]
    async fn a_stream_that_keeps_receiving_keep_alives_stays_open_and_the_caller_can_still_end_it()
    {
        let server = serve(|req, _| {
            if req.contains("/api/daemon/token") {
                return Some(token_ok());
            }
            // The response is split across writes in the handler below, so use a chunked body sent in one go with a gap handled by
            // the server task: here everything is sent at once, and the stream is held open afterwards.
            Some("HOLD:HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n: keep-alive\n\n: keep-alive\n\nid: 1\nevent: settings.update\ndata: {}\n\n".into())
        })
        .await;
        let c = conn(&server, REQUEST_TIMEOUT, Duration::from_millis(400));
        let mut s = c.stream(Some("0")).await.unwrap();
        let m = s.next().await.unwrap().unwrap();
        assert_eq!(
            (m.id.as_deref(), m.event.as_str()),
            (Some("1"), "settings.update")
        );
        // The caller ends it by dropping it; nothing is reported as quiet.
        drop(s);
    }

    #[tokio::test]
    async fn the_stream_sends_the_last_event_id_and_a_401_clears_the_token() {
        let seen = Arc::new(StdMutex::new(vec![]));
        let s2 = seen.clone();
        let server = serve(move |req, _| {
            if req.contains("/api/daemon/token") {
                return Some(token_ok());
            }
            s2.lock().unwrap().push(req.to_lowercase());
            Some(json_response(401, "{}"))
        })
        .await;
        let c = conn(&server, REQUEST_TIMEOUT, STREAM_IDLE);
        let e = c.stream(Some("41")).await.err().unwrap();
        assert_eq!(e.status(), Some(401));
        let req = seen.lock().unwrap()[0].clone();
        assert!(
            req.contains("last-event-id: 41") && req.contains("authorization: bearer whd_x"),
            "{req}"
        );
        assert!(c.cached.lock().await.is_none());
    }

    #[tokio::test]
    async fn a_401_buys_a_fresh_token_and_tries_once_more_and_the_token_is_cached() {
        let tokens = Arc::new(AtomicUsize::new(0));
        let t2 = tokens.clone();
        let server = serve(move |req, i| {
            if req.contains("/api/daemon/token") {
                t2.fetch_add(1, Ordering::SeqCst);
                return Some(token_ok());
            }
            // The first API call is refused; later ones succeed.
            Some(if i < 2 {
                json_response(401, "{}")
            } else {
                json_response(200, r#"{"ok":true}"#)
            })
        })
        .await;
        let c = conn(&server, REQUEST_TIMEOUT, STREAM_IDLE);
        assert_eq!(
            c.post_json("/api/daemon/heartbeat", &json!({}))
                .await
                .unwrap()["ok"],
            true
        );
        assert_eq!(tokens.load(Ordering::SeqCst), 2);
        c.post_json("/api/daemon/heartbeat", &json!({}))
            .await
            .unwrap();
        assert_eq!(tokens.load(Ordering::SeqCst), 2, "a good token is reused");
    }

    #[tokio::test]
    async fn a_refused_token_request_means_the_device_is_unpaired() {
        let server = serve(|_, _| Some(json_response(401, "{}"))).await;
        let c = conn(&server, REQUEST_TIMEOUT, STREAM_IDLE);
        assert_eq!(
            c.post_json("/x", &json!({})).await.unwrap_err(),
            ConnError::Unpaired
        );
    }

    #[tokio::test]
    async fn an_error_answer_carries_the_servers_code_and_message() {
        let server = serve(|req, _| {
            if req.contains("/api/daemon/token") {
                return Some(token_ok());
            }
            Some(json_response(
                409,
                r#"{"error":{"code":"job_finished","message":"it is over"}}"#,
            ))
        })
        .await;
        let c = conn(&server, REQUEST_TIMEOUT, STREAM_IDLE);
        assert_eq!(
            c.post_json("/x", &json!({})).await.unwrap_err(),
            ConnError::Http {
                status: 409,
                code: "job_finished".into(),
                message: "it is over".into()
            }
        );
    }

    // --- the reporter, against a stand-in poster --------------------------------------------------------------------------------

    #[derive(Default)]
    struct Fake {
        posts: StdMutex<Vec<(String, Value)>>,
        fail: StdMutex<VecDeque<ConnError>>,
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

    fn cfg() -> ReporterConfig {
        ReporterConfig {
            flush: Duration::from_millis(10),
            batch: 50,
            max_queued: 5,
            finish_retry: Duration::from_millis(5),
        }
    }

    fn http(status: u16) -> ConnError {
        ConnError::Http {
            status,
            code: "x".into(),
            message: "x".into(),
        }
    }

    #[tokio::test]
    async fn events_are_numbered_and_sent_in_a_batch_after_the_flush_delay() {
        let f = Arc::new(Fake::default());
        let r = JobReporter::new(f.clone(), "j1", None, cfg(), |_| {});
        r.event("a", json!({}));
        r.event("b", json!({ "k": 1 }));
        tokio::time::sleep(Duration::from_millis(80)).await;
        let posts = f.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, "/api/daemon/jobs/j1/events");
        let evs = posts[0].1["events"].as_array().unwrap();
        assert_eq!(
            evs.iter()
                .map(|e| e["seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(evs[1]["type"], "b");
        assert!(evs[0]["at"].as_str().unwrap().ends_with('Z'));
    }

    #[tokio::test]
    async fn a_failed_send_keeps_the_events_and_the_oldest_go_past_the_limit() {
        let f = Arc::new(Fake::default());
        f.fail
            .lock()
            .unwrap()
            .push_back(ConnError::Transport("down".into()));
        let r = JobReporter::new(f.clone(), "j1", Some("/api/daemon/chat/m1"), cfg(), |_| {});
        for i in 0..8 {
            r.event("e", json!({ "i": i }));
        }
        assert_eq!(r.queued(), 5, "only the newest five are kept");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let posts = f.posts.lock().unwrap();
        assert_eq!(posts[0].0, "/api/daemon/chat/m1/events");
        assert_eq!(posts[0].1["events"][0]["seq"], 3);
        assert_eq!(r.queued(), 0);
    }

    #[tokio::test]
    async fn a_finished_jobs_events_are_refused_for_good_and_dropped() {
        let f = Arc::new(Fake::default());
        f.fail.lock().unwrap().push_back(http(409));
        let r = JobReporter::new(f.clone(), "j1", None, cfg(), |_| {});
        r.event("a", json!({}));
        r.flush().await;
        assert_eq!(r.queued(), 0);
        assert!(f.posts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn finish_flushes_what_is_left_then_sends_the_final_state() {
        let f = Arc::new(Fake::default());
        let r = JobReporter::new(f.clone(), "j1", None, cfg(), |_| {});
        r.event("a", json!({}));
        let out = r.finish(json!({ "status": "succeeded" })).await;
        assert_eq!(
            out,
            FinishOutcome {
                sent: true,
                retry: false
            }
        );
        let posts = f.posts.lock().unwrap();
        assert_eq!(
            posts.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(),
            ["/api/daemon/jobs/j1/events", "/api/daemon/jobs/j1/finish"]
        );
    }

    #[tokio::test]
    async fn finish_retries_a_server_error_and_gives_up_on_a_refusal() {
        let f = Arc::new(Fake::default());
        f.fail
            .lock()
            .unwrap()
            .extend([http(503), ConnError::Timeout]);
        let r = JobReporter::new(f.clone(), "j1", None, cfg(), |_| {});
        assert_eq!(
            r.finish(json!({})).await,
            FinishOutcome {
                sent: true,
                retry: false
            }
        );

        f.fail.lock().unwrap().push_back(http(400));
        assert_eq!(
            r.finish(json!({})).await,
            FinishOutcome {
                sent: false,
                retry: false
            }
        );

        f.fail.lock().unwrap().push_back(http(429));
        f.fail
            .lock()
            .unwrap()
            .extend(std::iter::repeat_with(|| http(500)).take(4));
        assert_eq!(
            r.finish(json!({})).await,
            FinishOutcome {
                sent: false,
                retry: true
            }
        );
    }

    #[test]
    fn timestamps_parse_back_and_offsets_are_honoured() {
        for ms in [0u64, 951_782_400_000, 1_762_210_375_525, 4_102_444_800_123] {
            assert_eq!(parse_iso_ms(&iso_from_ms(ms)), Some(ms));
        }
        assert_eq!(
            parse_iso_ms("2030-01-01T02:00:00+02:00"),
            parse_iso_ms("2030-01-01T00:00:00Z")
        );
        assert_eq!(
            parse_iso_ms("2030-01-01T00:00:00Z"),
            Some(1_893_456_000_000)
        );
        assert_eq!(parse_iso_ms("not a date"), None);
        assert_eq!(parse_iso_ms("2030-13-01T00:00:00Z"), None);
    }

    #[test]
    fn iso_timestamps_match_javascripts_format() {
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_762_210_375_525), "2025-11-03T22:52:55.525Z");
        assert_eq!(iso_from_ms(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }
}
