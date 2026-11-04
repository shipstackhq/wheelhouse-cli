//! `wheelhouse daemon pair | run | status | unpair` (and the `wheelhouse-daemon` name for the same commands).
//! Port of `daemon/src/cli.mjs` and `main.mjs`. The CLI's own commands (board, login, doctor...) are WH-7, not this.

use crate::cancel::Cancel;
use crate::capabilities::node_platform;
use crate::config::{daemon_home, default_config, load_config, save_config, Paths};
use crate::connection::iso_now;
use crate::daemon::{run_daemon, DaemonOptions, Log, VERSION};
use crate::keystore::{delete_key, load_key, store_key};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Where a command's messages go.
pub type Say = Arc<dyn Fn(String) + Send + Sync>;

pub const USAGE: &str =
    "usage: wheelhouse-daemon pair --server <url> [--name NAME] | run | status | unpair";

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    /// `--flag`
    Flag,
    /// `--key value` or `--key=value`
    Value(String),
}

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub positional: Vec<String>,
    pub options: HashMap<String, Arg>,
}

impl Args {
    pub fn text(&self, key: &str) -> Option<&str> {
        match self.options.get(key) {
            Some(Arg::Value(v)) => Some(v),
            _ => None,
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.options.contains_key(key)
    }
}

/// `--key value`, `--key=value` and `--flag`; a following word that starts with `--` is not a value.
pub fn parse_args(argv: &[String]) -> Args {
    let mut out = Args::default();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if let Some(rest) = a.strip_prefix("--") {
            let (k, v) = match rest.split_once('=') {
                Some((k, v)) => (k, Some(v.to_string())),
                None => (rest, None),
            };
            let arg = match v {
                Some(v) => Arg::Value(v),
                None if argv.get(i + 1).is_some_and(|n| !n.starts_with("--")) => {
                    i += 1;
                    Arg::Value(argv[i].clone())
                }
                None => Arg::Flag,
            };
            out.options.insert(k.to_string(), arg);
        } else {
            out.positional.push(a.clone());
        }
        i += 1;
    }
    out
}

async fn post_json(client: &reqwest::Client, url: &str, body: &Value) -> Result<Value, String> {
    let res = client
        .post(url)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let json: Option<Value> = res.json().await.ok();
    if !status.is_success() {
        return Err(json
            .as_ref()
            .and_then(|j| j["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| format!("{url} answered {}", status.as_u16())));
    }
    Ok(json.unwrap_or(Value::Null))
}

/// A name for the machine people recognise: the Mac's own "computer name" when there is one, else the host name.
pub async fn machine_name() -> String {
    let run = |cmd: &'static str, args: &'static [&'static str]| async move {
        let out = tokio::process::Command::new(cmd)
            .args(args)
            .output()
            .await
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    if cfg!(target_os = "macos") {
        if let Some(n) = run("scutil", &["--get", "ComputerName"]).await {
            return n;
        }
    }
    let host = run("hostname", &[])
        .await
        .unwrap_or_else(|| "this machine".into());
    host.strip_suffix(".local").unwrap_or(&host).to_string()
}

pub struct PairOptions {
    pub paths: Paths,
    pub args: Args,
    /// Print and do not open the browser (tests, and `--no-open`).
    pub open_browser: bool,
    /// Where messages go.
    pub say: Say,
}

pub async fn pair(o: PairOptions) -> Result<(), String> {
    let server = o
        .args
        .text("server")
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    if !(server.starts_with("http://") || server.starts_with("https://")) {
        return Err(
            "usage: wheelhouse-daemon pair --server https://your-wheelhouse [--name NAME]".into(),
        );
    }
    let p = &o.paths;
    let before = load_config(p);
    if before.get("deviceId").is_some_and(|v| !v.is_null()) && !o.args.has("force") {
        return Err(format!(
            "Already paired with {} as {}. Run `wheelhouse-daemon unpair` first, or pass --force.",
            before["server"].as_str().unwrap_or("undefined"),
            before["name"].as_str().unwrap_or("null")
        ));
    }
    let keystore = if o.args.text("keystore") == Some("file") {
        "file".to_string()
    } else {
        before["keystore"].as_str().unwrap_or("file").to_string()
    };
    let name = match o.args.text("name") {
        Some(n) => n.to_string(),
        None => machine_name().await,
    };

    let key = wh_proto::keys::generate();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let start = post_json(&client, &format!("{server}/api/daemon/pair/start"), &json!({ "publicKey": wh_proto::keys::raw_public(&key.verifying_key()), "name": name, "os": node_platform() })).await?;
    let uri = start["verificationUriComplete"].as_str().unwrap_or("");
    (o.say)(format!("\nTo pair {name} with Wheelhouse, open:\n\n  {uri}\n\nand check that it shows this code: {}\n", start["userCode"].as_str().unwrap_or("")));
    if cfg!(target_os = "macos") && o.open_browser && !o.args.has("no-open") {
        let _ = std::process::Command::new("open")
            .arg(uri)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    let mut interval = Duration::from_secs_f64(start["interval"].as_f64().unwrap_or(5.0));
    let deadline =
        Instant::now() + Duration::from_secs_f64(start["expiresIn"].as_f64().unwrap_or(600.0));
    while Instant::now() < deadline {
        tokio::time::sleep(interval).await;
        let r = post_json(
            &client,
            &format!("{server}/api/daemon/pair/poll"),
            &json!({ "deviceCode": start["deviceCode"] }),
        )
        .await?;
        match r["status"].as_str() {
            Some("pending") => continue,
            Some("slow_down") => {
                interval += Duration::from_secs(5);
                continue;
            }
            Some("denied") => return Err("Pairing was refused in Wheelhouse.".into()),
            Some("expired") => break,
            Some("approved") => {
                store_key(&keystore, &p.home, &wh_proto::keys::export_private(&key))?;
                let mut next = before.clone();
                for (k, v) in [
                    ("server", json!(server)),
                    ("keystore", json!(keystore)),
                    ("deviceId", r["deviceId"].clone()),
                    ("serverPublicKey", r["serverPublicKey"].clone()),
                    ("name", r["device"]["name"].clone()),
                    ("poolSize", r["device"]["poolSize"].clone()),
                    ("lastEventId", Value::Null),
                ] {
                    next.insert(k.into(), v);
                }
                save_config(&next, p).map_err(|e| e.to_string())?;
                let name = r["device"]["name"].as_str().unwrap_or("");
                let where_ = if keystore == "keychain" {
                    "login Keychain".to_string()
                } else {
                    format!("file {}/device-key", p.home.display())
                };
                (o.say)(format!("Paired as \"{name}\". The key is in the {where_}."));
                (o.say)("Start it now with `wheelhouse-daemon run`, or install it as a background service:\n  sh daemon/platform/macos/install.sh --bin \"$(command -v wheelhouse-daemon)\"".into());
                return Ok(());
            }
            _ => {}
        }
    }
    Err("The code expired before it was approved. Run pair again.".into())
}

pub fn status(p: &Paths) -> String {
    let c = load_config(p);
    if c["deviceId"].is_null() {
        return format!("Not paired. Settings would live in {}.", p.home.display());
    }
    let keystore = c["keystore"].as_str().unwrap_or("file");
    let key = if load_key(keystore, &p.home).is_some() {
        format!("in the {keystore}")
    } else {
        format!("MISSING from the {keystore}")
    };
    [
        format!("wheelhouse-daemon {VERSION}"),
        format!("server:     {}", c["server"].as_str().unwrap_or("")),
        format!(
            "device:     {} ({})",
            c["name"].as_str().unwrap_or(""),
            c["deviceId"].as_str().unwrap_or("")
        ),
        format!("runs:       up to {} at once", c["poolSize"]),
        format!("key:        {key}"),
        format!("home:       {}", p.home.display()),
    ]
    .join("\n")
}

/// Forget the pairing and the key here. What is kept: the keystore choice, claude path, git protocol, runner policy and repositories.
pub fn unpair(p: &Paths) -> Result<String, String> {
    let c = load_config(p);
    delete_key(c["keystore"].as_str().unwrap_or("file"), &p.home);
    let mut next: Map<String, Value> = default_config();
    for k in [
        "keystore",
        "claudePath",
        "gitProtocol",
        "runner",
        "repositories",
    ] {
        if let Some(v) = c.get(k) {
            next.insert(k.into(), v.clone());
        }
    }
    save_config(&next, p).map_err(|e| e.to_string())?;
    let of = c["name"]
        .as_str()
        .map(|n| format!(" of {n}"))
        .unwrap_or_default();
    Ok(format!("Forgot the pairing{of} and its key. Unpair it in Wheelhouse (Settings → Devices) as well, if it is still listed."))
}

fn stamped_log() -> Log {
    Arc::new(|m| println!("{} {m}", iso_now()))
}

#[cfg(unix)]
async fn signalled(stop: Cancel, log: Log) -> Arc<std::sync::Mutex<Option<&'static str>>> {
    use tokio::signal::unix::{signal, SignalKind};
    let got = Arc::new(std::sync::Mutex::new(None));
    let (got2, stop2) = (got.clone(), stop.clone());
    if let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) {
        tokio::spawn(async move {
            let name = tokio::select! { _ = term.recv() => "SIGTERM", _ = int.recv() => "SIGINT" };
            *got2.lock().unwrap() = Some(name);
            log(format!("{name}: stopping, canceling running jobs"));
            stop2.cancel();
        });
    }
    got
}

#[cfg(not(unix))]
async fn signalled(stop: Cancel, log: Log) -> Arc<std::sync::Mutex<Option<&'static str>>> {
    let got = Arc::new(std::sync::Mutex::new(None));
    let got2 = got.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            *got2.lock().unwrap() = Some("SIGINT");
            log("SIGINT: stopping, canceling running jobs".into());
            stop.cancel();
        }
    });
    got
}

/// Run the daemon in the foreground. Only being unpaired is a clean exit: the background service then stays stopped. Any other stop, a
/// signal included, exits as a failure (the shell's 128 + signal number), so the service starts the daemon again. Deliberate stops
/// (launchctl bootout, the uninstall script) remove the service first, so they are not restarted.
pub async fn run(p: Paths) -> i32 {
    let (stop, log) = (Cancel::new(), stamped_log());
    let got = signalled(stop.clone(), log.clone()).await;
    match run_daemon(DaemonOptions::new(p, stop, log)).await {
        Ok(exit) if exit.unpaired => 0,
        Ok(_) => match *got.lock().unwrap() {
            Some("SIGINT") => 130,
            Some("SIGTERM") => 143,
            _ => 1,
        },
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// The daemon commands. `argv` is what follows the command name (`pair --server ...`, `run`...). Returns the exit code.
pub async fn main(argv: Vec<String>) -> i32 {
    let o = parse_args(&argv);
    let cmd = o.positional.first().map(String::as_str);
    let p = Paths::new(daemon_home());
    let fail = |e: String| {
        eprintln!("{e}");
        1
    };
    match cmd {
        Some("pair") => {
            let say: Say = Arc::new(|m| println!("{m}"));
            match pair(PairOptions {
                paths: p,
                args: o,
                open_browser: true,
                say,
            })
            .await
            {
                Ok(()) => 0,
                Err(e) => fail(e),
            }
        }
        Some("status") => {
            println!("{}", status(&p));
            0
        }
        Some("unpair") => match unpair(&p) {
            Ok(m) => {
                println!("{m}");
                0
            }
            Err(e) => fail(e),
        },
        Some("run") => run(p).await,
        Some("version") => {
            println!("{VERSION}");
            0
        }
        _ if o.has("version") => {
            println!("{VERSION}");
            0
        }
        _ => {
            println!("{USAGE}");
            if cmd.is_some() && cmd != Some("help") {
                2
            } else {
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn options_are_parsed_as_the_node_cli_does() {
        let a = parse_args(&argv(
            "pair --server https://x --name=Mac --force --no-open",
        ));
        assert_eq!(a.positional, ["pair"]);
        assert_eq!(
            (a.text("server"), a.text("name")),
            (Some("https://x"), Some("Mac"))
        );
        assert_eq!(
            a.options.get("force"),
            Some(&Arg::Flag),
            "a flag followed by another option has no value"
        );
        assert!(a.has("no-open") && !a.has("keystore"));
        // A flag at the end, and a value that is the next positional word.
        assert_eq!(
            parse_args(&argv("run --verbose")).options.get("verbose"),
            Some(&Arg::Flag)
        );
        assert_eq!(
            parse_args(&argv("--keystore file pair")).text("keystore"),
            Some("file")
        );
    }

    async fn mock(handler: impl Fn(&str) -> String + Send + Sync + 'static) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let h = Arc::new(handler);
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                let h = h.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    s.write_all(h(&req).as_bytes()).await.ok();
                });
            }
        });
        format!("http://{addr}")
    }

    fn resp(status: u16, body: &str) -> String {
        format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
    }

    fn said() -> (Say, Arc<Mutex<Vec<String>>>) {
        let v = Arc::new(Mutex::new(vec![]));
        let v2 = v.clone();
        (Arc::new(move |m| v2.lock().unwrap().push(m)), v)
    }

    fn opts(
        p: &Paths,
        server: &str,
        extra: &str,
        say: Arc<dyn Fn(String) + Send + Sync>,
    ) -> PairOptions {
        PairOptions {
            paths: p.clone(),
            args: parse_args(&argv(&format!(
                "pair --server {server} --keystore file --name Test {extra}"
            ))),
            open_browser: false,
            say,
        }
    }

    #[tokio::test]
    async fn pairing_asks_wheelhouse_waits_for_approval_and_keeps_the_key_and_the_settings() {
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (polls2, seen) = (polls.clone(), Arc::new(Mutex::new(vec![])));
        let seen2 = seen.clone();
        let server_key = wh_proto::keys::generate();
        let spk = wh_proto::keys::raw_public(&server_key.verifying_key());
        let spk_for_server = spk.clone();
        let server = mock(move |req| {
            seen2.lock().unwrap().push(req.to_string());
            if req.starts_with("POST /api/daemon/pair/start") {
                resp(200, r#"{"verificationUriComplete":"https://wh.test/pair?c=AB","userCode":"AB-12","deviceCode":"dc1","interval":0,"expiresIn":30}"#)
            } else {
                let n = polls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                match n {
                    0 => resp(200, r#"{"status":"pending"}"#),
                    _ => resp(200, &format!(r#"{{"status":"approved","deviceId":"dev-1","serverPublicKey":"{spk_for_server}","device":{{"name":"Test Mac","poolSize":2}}}}"#)),
                }
            }
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path().join("d"));
        let (say, lines) = said();
        pair(opts(&p, &server, "", say)).await.unwrap();

        let start = seen.lock().unwrap()[0].clone();
        assert!(
            start.contains("\"publicKey\"")
                && start.contains("\"name\":\"Test\"")
                && start.contains("\"os\":"),
            "{start}"
        );
        let c = load_config(&p);
        assert_eq!(
            (
                c["deviceId"].clone(),
                c["name"].clone(),
                c["poolSize"].clone(),
                c["server"].clone()
            ),
            (json!("dev-1"), json!("Test Mac"), json!(2), json!(server))
        );
        assert_eq!(c["serverPublicKey"], json!(spk));
        let secret = load_key("file", &p.home).expect("the key is stored");
        // The stored key is the one whose public half was sent for approval.
        let sent = start
            .split("\"publicKey\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string();
        assert_eq!(
            wh_proto::keys::raw_public(
                &wh_proto::keys::import_private(&secret)
                    .unwrap()
                    .verifying_key()
            ),
            sent
        );
        let said = lines.lock().unwrap().join("\n");
        assert!(
            said.contains("https://wh.test/pair?c=AB")
                && said.contains("AB-12")
                && said.contains("Paired as \"Test Mac\""),
            "{said}"
        );

        // Already paired: refused without --force, allowed with it.
        let (say, _) = said_();
        let e = pair(opts(&p, &server, "", say.clone())).await.unwrap_err();
        assert!(
            e.contains("Already paired with") && e.contains("as Test Mac") && e.contains("--force"),
            "{e}"
        );
        polls.store(1, std::sync::atomic::Ordering::SeqCst);
        pair(opts(&p, &server, "--force", say)).await.unwrap();
    }

    fn said_() -> (Say, ()) {
        (Arc::new(|_| {}), ())
    }

    #[tokio::test]
    async fn a_refused_or_expired_code_or_a_bad_url_ends_pairing_with_a_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path().join("d"));
        let (say, _) = said_();
        let e = pair(PairOptions {
            paths: p.clone(),
            args: parse_args(&argv("pair --server ftp://x")),
            open_browser: false,
            say: say.clone(),
        })
        .await
        .unwrap_err();
        assert!(e.starts_with("usage: wheelhouse-daemon pair --server"));

        for (status, want) in [
            ("denied", "Pairing was refused in Wheelhouse."),
            (
                "expired",
                "The code expired before it was approved. Run pair again.",
            ),
        ] {
            let server = mock(move |req| {
                if req.starts_with("POST /api/daemon/pair/start") {
                    resp(200, r#"{"verificationUriComplete":"u","userCode":"c","deviceCode":"d","interval":0,"expiresIn":30}"#)
                } else {
                    resp(200, &format!(r#"{{"status":"{status}"}}"#))
                }
            })
            .await;
            assert_eq!(
                pair(opts(&p, &server, "", say.clone())).await.unwrap_err(),
                want
            );
            assert!(load_config(&p)["deviceId"].is_null(), "nothing is saved");
        }
        let server = mock(|_| resp(400, r#"{"error":{"message":"bad key"}}"#)).await;
        assert_eq!(
            pair(opts(&p, &server, "", say)).await.unwrap_err(),
            "bad key"
        );
    }

    #[test]
    fn status_and_unpair_keep_what_belongs_to_the_machine() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path().join("d"));
        assert!(status(&p).starts_with("Not paired. Settings would live in"));
        store_key("file", &p.home, "secret").unwrap();
        let mut c = load_config(&p);
        for (k, v) in [
            ("server", json!("https://wh.test")),
            ("deviceId", json!("dev-1")),
            ("name", json!("Test Mac")),
            ("poolSize", json!(2)),
            ("keystore", json!("file")),
            ("gitProtocol", json!("ssh")),
            ("claudePath", json!("/x/claude")),
            ("lastEventId", json!("9")),
        ] {
            c.insert(k.into(), v);
        }
        save_config(&c, &p).unwrap();
        let s = status(&p);
        assert!(
            s.contains("server:     https://wh.test")
                && s.contains("device:     Test Mac (dev-1)")
                && s.contains("runs:       up to 2 at once")
                && s.contains("key:        in the file"),
            "{s}"
        );

        let msg = unpair(&p).unwrap();
        assert!(msg.contains("Forgot the pairing of Test Mac"));
        assert!(load_key("file", &p.home).is_none());
        let after = load_config(&p);
        assert!(
            after["deviceId"].is_null()
                && after["server"].is_null()
                && after["lastEventId"].is_null()
        );
        assert_eq!(
            (after["gitProtocol"].clone(), after["claudePath"].clone()),
            (json!("ssh"), json!("/x/claude")),
            "machine settings stay"
        );
        assert!(status(&p).starts_with("Not paired"));
        assert!(unpair(&p).is_ok(), "unpairing twice is harmless");
    }

    #[tokio::test]
    async fn the_commands_exit_with_the_codes_the_service_relies_on() {
        assert_eq!(main(argv("bogus")).await, 2);
        assert_eq!(main(argv("help")).await, 0);
        assert_eq!(main(vec![]).await, 0);
        assert_eq!(main(argv("version")).await, 0);
    }
}
