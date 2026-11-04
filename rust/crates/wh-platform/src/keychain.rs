//! The macOS login Keychain, through `security`. Port of `cli/lib/keychain.mjs`. Elsewhere callers keep a file instead.

use std::io::Write;
use std::process::{Command, Stdio};

pub fn get(service: &str, account: &str) -> Option<String> {
    let out = Command::new("security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// `security -i` reads the command from stdin, so the secret never shows in the process list as an argument would.
pub fn set(service: &str, account: &str, label: &str, secret: &str) -> Result<(), String> {
    for (name, v) in [
        ("service", service),
        ("account", account),
        ("label", label),
        ("secret", secret),
    ] {
        if v.contains(['"', '\n', '\r', '\\']) {
            return Err(format!(
                "the Keychain {name} has a character that cannot be passed safely"
            ));
        }
    }
    let mut child = Command::new("security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run security: {e}"))?;
    let line = format!("add-generic-password -U -s \"{service}\" -a \"{account}\" -l \"{label}\" -w \"{secret}\"\n");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(if out.stderr.is_empty() {
            &out.stdout
        } else {
            &out.stderr
        })
        .trim()
        .to_string();
        return Err(format!("could not save the key in the Keychain: {why}"));
    }
    Ok(())
}

pub fn delete(service: &str, account: &str) {
    let _ = Command::new("security")
        .args(["delete-generic-password", "-s", service, "-a", account])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Touches the real login Keychain under a throwaway service name; skipped where the Keychain is not usable (headless CI).
    #[test]
    fn set_get_and_delete_round_trip() {
        let service = format!("com.shipstack.wh-test-{}", std::process::id());
        if set(&service, "t", "wh test", "abc123").is_err() {
            eprintln!("skipped: Keychain not usable here");
            return;
        }
        assert_eq!(get(&service, "t").as_deref(), Some("abc123"));
        delete(&service, "t");
        assert_eq!(get(&service, "t"), None);
    }
}
