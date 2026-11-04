//! What this machine can run right now, as the daemon reports it on every connect. Everything comes from asking the claude CLI itself
//! (`claude --version`, `claude auth status`), never from reading its files, and only the fields Wheelhouse uses are sent: no email,
//! no organisation. Port of `daemon/src/capabilities.mjs`.

use serde_json::{json, Value};
use std::time::Duration;
use tokio::process::Command;

/// The model aliases every Claude Code install understands; the plan decides which ones actually answer.
pub const MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];

async fn out(cmd: &str, args: &[&str], limit: Duration) -> Option<String> {
    let child = Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let o = tokio::time::timeout(limit, child).await.ok()?.ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// The leading digits and dots of a version line (`2.1.3 (Claude Code)` -> `2.1.3`), else its first 40 characters.
fn short_version(v: &str) -> String {
    let n = v
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(v.len());
    if n > 0 {
        v[..n].to_string()
    } else {
        v.chars().take(40).collect()
    }
}

pub fn claude_report(version: Option<&str>, status: Option<&Value>) -> Value {
    let Some(version) = version else {
        return json!({ "installed": false, "version": null, "signedIn": false, "subscriptionType": null, "models": [] });
    };
    let signed_in = status.map(|s| s["loggedIn"] == true).unwrap_or(false);
    let sub = status
        .and_then(|s| s["subscriptionType"].as_str())
        .filter(|_| signed_in);
    json!({
        "installed": true,
        "version": short_version(version),
        "signedIn": signed_in,
        "subscriptionType": sub,
        "models": if signed_in { json!(MODELS) } else { json!([]) },
    })
}

pub async fn probe_claude(claude_path: &str) -> Value {
    let version = out(claude_path, &["--version"], Duration::from_secs(15)).await;
    if version.is_none() {
        return claude_report(None, None);
    }
    // An older CLI without `auth status` leaves this unknown.
    let status = out(
        claude_path,
        &["auth", "status", "--json"],
        Duration::from_secs(15),
    )
    .await
    .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    claude_report(version.as_deref(), status.as_ref())
}

/// On a Mac, whether it is running on battery (`pmset`); false elsewhere.
pub async fn on_battery() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    out("pmset", &["-g", "batt"], Duration::from_secs(5))
        .await
        .map(|t| t.contains("Battery Power"))
        .unwrap_or(false)
}

pub async fn capabilities(claude_path: &str, held_repositories: &[String]) -> Value {
    json!({
        "claude": probe_claude(claude_path).await,
        "repositories": held_repositories.iter().map(|r| json!({ "repositoryId": r, "source": "clone" })).collect::<Vec<_>>(),
        "platform": node_platform(),
        "power": { "onBattery": on_battery().await },
    })
}

/// The value of Node's `process.platform`, which the server already sees from the Node daemon.
pub fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_claude_is_reported_as_not_installed() {
        let r = claude_report(None, None);
        assert_eq!(
            r,
            json!({ "installed": false, "version": null, "signedIn": false, "subscriptionType": null, "models": [] })
        );
    }

    #[test]
    fn signed_in_shows_the_plan_and_the_models_and_nothing_else() {
        let status = json!({ "loggedIn": true, "subscriptionType": "max", "email": "a@b.c", "orgName": "x" });
        let r = claude_report(Some("2.1.3 (Claude Code)"), Some(&status));
        assert_eq!(
            r,
            json!({ "installed": true, "version": "2.1.3", "signedIn": true, "subscriptionType": "max", "models": ["opus", "sonnet", "haiku"] })
        );
    }

    #[test]
    fn signed_out_or_unknown_status_has_no_plan_and_no_models() {
        let r = claude_report(
            Some("1.0.0"),
            Some(&json!({ "loggedIn": false, "subscriptionType": "pro" })),
        );
        assert_eq!(
            (
                r["signedIn"].clone(),
                r["subscriptionType"].clone(),
                r["models"].clone()
            ),
            (json!(false), json!(null), json!([]))
        );
        assert_eq!(claude_report(Some("1.0.0"), None)["signedIn"], false);
        assert_eq!(claude_report(Some("weird"), None)["version"], "weird");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_asks_the_claude_cli_itself() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("claude");
        std::fs::write(&fake, "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '3.0.1 (Claude Code)'; else echo '{\"loggedIn\":true,\"subscriptionType\":\"pro\"}'; fi\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = probe_claude(fake.to_str().unwrap()).await;
        assert_eq!(
            (r["version"].clone(), r["subscriptionType"].clone()),
            (json!("3.0.1"), json!("pro"))
        );
        assert_eq!(
            probe_claude("/nonexistent/claude").await["installed"],
            false
        );
    }
}
