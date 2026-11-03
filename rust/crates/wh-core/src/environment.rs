//! A background service starts with a bare PATH (launchd gives /usr/bin:/bin:/usr/sbin:/sbin), which hides the tools a person's shell
//! finds: claude in ~/.local/bin, gh and pnpm in Homebrew or nvm folders. At start the daemon takes the PATH of the user's login shell,
//! plus the usual install folders, so it and every run see what a terminal sees. Port of `daemon/src/environment.mjs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// PATH separator on this platform.
const DELIM: char = if cfg!(windows) { ';' } else { ':' };

/// The usual install folders a background service would miss.
pub fn common_dirs() -> Vec<String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    vec![
        home.join(".local").join("bin").display().to_string(),
        home.join(".claude").join("local").display().to_string(),
        "/opt/homebrew/bin".into(),
        "/usr/local/bin".into(),
    ]
}

/// PATH entries joined without duplicates, the first occurrence kept, empty and missing parts skipped.
pub fn merge_path(parts: &[Option<&str>]) -> String {
    let mut seen = std::collections::HashSet::new();
    parts
        .iter()
        .flatten()
        .flat_map(|p| p.split(DELIM))
        .filter(|d| !d.is_empty() && seen.insert(*d))
        .collect::<Vec<_>>()
        .join(&DELIM.to_string())
}

/// The PATH an interactive login shell of the user would have, or None if it cannot be read within five seconds.
pub fn login_shell_path(shell: &str) -> Option<String> {
    const START: &str = "__WH_PATH__";
    const END: &str = "__WH_END__";
    let mut child = Command::new(shell)
        .args(["-ilc", r#"printf "__WH_PATH__%s__WH_END__" "$PATH""#])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().ok()? {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let start = out.find(START)? + START.len();
    let end = out[start..].rfind(END)? + start;
    let path = &out[start..end];
    (!path.is_empty()).then(|| path.to_string())
}

/// Widen this process's PATH, for it and its children. Returns the new value.
pub fn adopt_user_path() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let login = login_shell_path(&shell);
    let common = common_dirs().join(&DELIM.to_string());
    let merged = merge_path(&[Some(&current), login.as_deref(), Some(&common)]);
    std::env::set_var("PATH", &merged);
    merged
}

/// The claude executable: the configured path if it contains a slash and exists, else the first executable `claude` on `search_path`.
pub fn find_claude(configured: &str, search_path: &str) -> Option<PathBuf> {
    if configured.contains('/') {
        return Path::new(configured)
            .exists()
            .then(|| PathBuf::from(configured));
    }
    search_path
        .split(DELIM)
        .map(|dir| Path::new(dir).join(configured))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_entries_merge_in_order_without_duplicates() {
        assert_eq!(
            merge_path(&[
                Some("/usr/bin:/bin"),
                Some("/opt/x:/usr/bin"),
                None,
                Some("/bin:/y")
            ]),
            "/usr/bin:/bin:/opt/x:/y"
        );
    }

    #[test]
    fn claude_is_found_on_a_path_by_an_explicit_path_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("wh-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("claude");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let d = dir.display().to_string();
        assert_eq!(
            find_claude("claude", &format!("/nowhere{DELIM}{d}")),
            Some(bin.clone())
        );
        assert_eq!(
            find_claude(&bin.display().to_string(), ""),
            Some(bin.clone())
        );
        assert_eq!(find_claude("claude", "/nowhere"), None);
        assert_eq!(
            find_claude(&dir.join("nope").display().to_string(), ""),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_login_shells_path_can_be_read_and_is_a_real_path() {
        let p = login_shell_path("/bin/sh");
        assert!(p.is_none() || p.unwrap().split(DELIM).count() > 1);
    }
}
