//! Spike: token exchange + SSE read with ureq (blocking, platform verifier) on a plain thread.
//! Usage: spike-ureq-threads <server-url> <device-id> <private-key-base64> [max-messages]

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};
use wh_proto::{keys, make_assertion, SseParser};

fn run(server: String, device: String, key_b64: String, max: usize) -> Result<(), Box<dyn std::error::Error>> {
    let key = keys::import_private(&key_b64)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

    let body: serde_json::Value = ureq::post(format!("{server}/api/daemon/token"))
        .send_json(serde_json::json!({ "assertion": make_assertion(&device, &key, now) }))?
        .body_mut()
        .read_json()?;
    let token = body["accessToken"].as_str().ok_or("no accessToken")?.to_string();

    let mut res = ureq::get(format!("{server}/api/daemon/stream"))
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "text/event-stream")
        .call()?;
    let mut reader = res.body_mut().as_reader();
    let (mut parser, mut seen, mut buf) = (SseParser::new(), 0, [0u8; 4096]);
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        for m in parser.push(&String::from_utf8_lossy(&buf[..n])) {
            println!("{} {}", m.event, m.data.len());
            seen += 1;
        }
        if seen >= max {
            return Ok(());
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: {} <server-url> <device-id> <private-key-base64> [max-messages]", a[0]);
        std::process::exit(2);
    }
    let max = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
    let server = a[1].trim_end_matches('/').to_string();
    // The stream runs on its own thread, as the daemon's connection loop would.
    let h = std::thread::spawn(move || run(server, a[2].clone(), a[3].clone(), max).map_err(|e| e.to_string()));
    if let Err(e) = h.join().expect("stream thread panicked") {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
