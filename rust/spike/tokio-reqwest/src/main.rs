//! Spike: token exchange + SSE read with tokio + reqwest (rustls, native roots).
//! Usage: spike-tokio-reqwest <server-url> <device-id> <private-key-base64> [max-messages]

use std::time::{SystemTime, UNIX_EPOCH};
use wh_proto::{keys, make_assertion, SseParser};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: {} <server-url> <device-id> <private-key-base64> [max-messages]", a[0]);
        std::process::exit(2);
    }
    let (server, device, key) = (a[1].trim_end_matches('/'), &a[2], keys::import_private(&a[3])?);
    let max: usize = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .post(format!("{server}/api/daemon/token"))
        .json(&serde_json::json!({ "assertion": make_assertion(device, &key, now) }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = body["accessToken"].as_str().ok_or("no accessToken")?;

    let mut res = client
        .get(format!("{server}/api/daemon/stream"))
        .bearer_auth(token)
        .header("accept", "text/event-stream")
        .send()
        .await?
        .error_for_status()?;
    let (mut parser, mut seen) = (SseParser::new(), 0);
    while let Some(chunk) = res.chunk().await? {
        for m in parser.push(&String::from_utf8_lossy(&chunk)) {
            println!("{} {}", m.event, m.data.len());
            seen += 1;
        }
        if seen >= max {
            break;
        }
    }
    Ok(())
}
