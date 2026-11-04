//! What a run may do without asking. Mirrors `DEFAULT_RUNNER` in `daemon/src/config.mjs`; the golden test keeps them equal.

use serde_json::{json, Value};

/// The runner policy: which tools exist, the permission mode, one allow or deny rule per command, and the time limit.
#[derive(Debug, Clone, PartialEq)]
pub struct RunnerConfig {
    pub tools: Vec<String>,
    pub permission_mode: String,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub timeout_minutes: u64,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The defaults a daemon starts with. A config file only overrides what differs from these.
pub fn default_runner() -> RunnerConfig {
    RunnerConfig {
        tools: strings(&["Read", "Edit", "Write", "Glob", "Grep", "Bash", "TodoWrite"]),
        permission_mode: "acceptEdits".into(),
        allowed_tools: strings(&[
            // Looking around (read only)
            "Bash(pwd)",
            "Bash(ls:*)",
            "Bash(cat:*)",
            "Bash(head:*)",
            "Bash(tail:*)",
            "Bash(wc:*)",
            "Bash(od:*)",
            "Bash(echo:*)",
            "Bash(find:*)",
            "Bash(grep:*)",
            "Bash(rg:*)",
            "Bash(diff:*)",
            // git, on this branch only
            "Bash(git status:*)",
            "Bash(git diff:*)",
            "Bash(git log:*)",
            "Bash(git show:*)",
            "Bash(git branch --show-current)",
            "Bash(git rev-parse:*)",
            "Bash(git ls-files:*)",
            "Bash(git blame:*)",
            "Bash(git add:*)",
            "Bash(git commit:*)",
            "Bash(git mv:*)",
            "Bash(git rm:*)",
            // Installing, building and checking the project
            "Bash(npm install:*)",
            "Bash(npm ci:*)",
            "Bash(npm test:*)",
            "Bash(npm run:*)",
            "Bash(npx tsc:*)",
            "Bash(npx eslint:*)",
            "Bash(npx prettier:*)",
            "Bash(pnpm install:*)",
            "Bash(pnpm test:*)",
            "Bash(pnpm run:*)",
            "Bash(pnpm typecheck:*)",
            "Bash(pnpm lint:*)",
            "Bash(pnpm build:*)",
            "Bash(pnpm --filter:*)",
            "Bash(pnpm exec tsc:*)",
            "Bash(pnpm exec eslint:*)",
            "Bash(yarn install:*)",
            "Bash(yarn test:*)",
            "Bash(yarn run:*)",
            "Bash(node --test:*)",
            "Bash(npx tsx --test:*)",
        ]),
        disallowed_tools: strings(&[
            "Bash(git push:*)",
            "Bash(git checkout:*)",
            "Bash(git switch:*)",
            "Bash(git reset:*)",
            "Bash(git rebase:*)",
            "Bash(git branch -d:*)",
            "Bash(git branch -D:*)",
            "Bash(git worktree:*)",
            "Bash(git remote:*)",
            "Bash(git config:*)",
            "Bash(rm -rf:*)",
            "Bash(curl:*)",
            "Bash(wget:*)",
            "Bash(ssh:*)",
            "Bash(sudo:*)",
        ]),
        timeout_minutes: 45,
    }
}

impl RunnerConfig {
    /// From the loaded config's `runner` object; anything missing or of the wrong type keeps the default.
    pub fn from_json(v: &Value) -> RunnerConfig {
        let d = default_runner();
        let list = |k: &str, fallback: Vec<String>| -> Vec<String> {
            v[k].as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or(fallback)
        };
        RunnerConfig {
            tools: list("tools", d.tools),
            permission_mode: v["permissionMode"]
                .as_str()
                .map(String::from)
                .unwrap_or(d.permission_mode),
            allowed_tools: list("allowedTools", d.allowed_tools),
            disallowed_tools: list("disallowedTools", d.disallowed_tools),
            timeout_minutes: v["timeoutMinutes"].as_u64().unwrap_or(d.timeout_minutes),
        }
    }

    /// The JSON shape the Node config uses (camelCase keys), for saving and for comparing with the daemon.
    pub fn to_json(&self) -> Value {
        json!({
            "tools": self.tools,
            "permissionMode": self.permission_mode,
            "allowedTools": self.allowed_tools,
            "disallowedTools": self.disallowed_tools,
            "timeoutMinutes": self.timeout_minutes,
        })
    }
}

// ---- Files: where the daemon keeps its things, and its settings file (port of the rest of daemon/src/config.mjs).

use serde_json::Map;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The daemon's home folder: WHEELHOUSE_DAEMON_HOME when set (tests use it), else ~/.wheelhouse/daemon.
pub fn daemon_home() -> PathBuf {
    if let Some(h) = std::env::var_os("WHEELHOUSE_DAEMON_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join(".wheelhouse").join("daemon")
}

/// Every folder and file under the daemon's home.
#[derive(Debug, Clone, PartialEq)]
pub struct Paths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub repos: PathBuf,
    pub worktrees: PathBuf,
    pub chats: PathBuf,
    pub jobs: PathBuf,
    pub logs: PathBuf,
}

impl Paths {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let home: PathBuf = home.into();
        Paths {
            config: home.join("config.json"),
            repos: home.join("repos"),
            worktrees: home.join("worktrees"),
            chats: home.join("chats"),
            jobs: home.join("state").join("jobs"),
            logs: home.join("logs"),
            home,
        }
    }
}

/// The settings a daemon starts with. The keystore default follows the platform, as in Node.
pub fn default_config() -> Map<String, Value> {
    let keystore = if cfg!(target_os = "macos") {
        "keychain"
    } else {
        "file"
    };
    let mut m = Map::new();
    m.insert("server".into(), Value::Null);
    m.insert("deviceId".into(), Value::Null);
    m.insert("serverPublicKey".into(), Value::Null);
    m.insert("name".into(), Value::Null);
    m.insert("poolSize".into(), json!(3));
    m.insert("chatModel".into(), Value::Null);
    m.insert("lastEventId".into(), Value::Null);
    m.insert("keystore".into(), json!(keystore));
    m.insert("claudePath".into(), json!("claude"));
    m.insert("gitProtocol".into(), json!("https"));
    m.insert("runner".into(), default_runner().to_json());
    m.insert("repositories".into(), json!({}));
    m.insert("keepFailedHours".into(), json!(24));
    m
}

/// The settings as the daemon uses them: the defaults, overridden by the saved file. A saved `runner` is merged over the default
/// runner's keys, so a newer daemon's runner defaults reach an existing machine.
pub fn load_config(p: &Paths) -> Map<String, Value> {
    let saved: Map<String, Value> = std::fs::read_to_string(&p.config)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            if let Value::Object(m) = v {
                Some(m)
            } else {
                None
            }
        })
        .unwrap_or_default();
    let mut out = default_config();
    let mut runner = default_runner()
        .to_json()
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (k, v) in saved.iter() {
        if k == "runner" {
            if let Some(r) = v.as_object() {
                for (rk, rv) in r {
                    runner.insert(rk.clone(), rv.clone());
                }
            }
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    out.insert("runner".into(), Value::Object(runner));
    out
}

/// Only what differs from the defaults is saved. A key the defaults do not know is always saved, so nothing is dropped.
fn overrides(config: &Map<String, Value>) -> Map<String, Value> {
    let defaults = default_config();
    config
        .iter()
        .filter(|(k, v)| match defaults.get(*k) {
            Some(d) => {
                // Compared as JSON text, key order included, as the Node code compares JSON.stringify output.
                let (saved_default, saved_value) = (d.to_string(), v.to_string());
                saved_default != saved_value
            }
            None => true,
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Write the settings atomically (a crash mid-write never leaves half a file), readable only by this user.
pub fn save_config(config: &Map<String, Value>, p: &Paths) -> std::io::Result<()> {
    std::fs::create_dir_all(&p.home)?;
    let mut text = serde_json::to_string_pretty(&Value::Object(overrides(config)))?;
    text.push('\n');
    let tmp = PathBuf::from(format!("{}.{}.tmp", p.config.display(), std::process::id()));
    write_private(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, &p.config)
}

/// `{ ...loadConfig(), ...patch }`, saved, and returned.
pub fn update_config(patch: Map<String, Value>, p: &Paths) -> std::io::Result<Map<String, Value>> {
    let mut next = load_config(p);
    next.extend(patch);
    save_config(&next, p)?;
    Ok(next)
}

/// Create or overwrite a file that only this user can read or write (0600 on Unix).
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)
}

#[cfg(test)]
mod file_tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wh-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn only_settings_that_differ_from_the_defaults_are_saved() {
        let home = scratch("config");
        let p = Paths::new(&home);
        let mut c = load_config(&p);
        c.insert("server".into(), json!("https://wh.example"));
        c.insert("poolSize".into(), json!(2));
        save_config(&c, &p).unwrap();
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&p.config).unwrap()).unwrap();
        assert_eq!(
            saved,
            json!({"server": "https://wh.example", "poolSize": 2})
        );
        assert_eq!(load_config(&p)["runner"], default_runner().to_json());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unknown_keys_survive_a_save() {
        let home = scratch("unknown");
        let p = Paths::new(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(&p.config, r#"{"futureSetting": {"a": 1}}"#).unwrap();
        update_config(Map::from_iter([("name".to_string(), json!("laptop"))]), &p).unwrap();
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&p.config).unwrap()).unwrap();
        assert_eq!(saved, json!({"futureSetting": {"a": 1}, "name": "laptop"}));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_saved_runner_is_merged_over_the_defaults() {
        let home = scratch("runner");
        let p = Paths::new(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(&p.config, r#"{"runner": {"timeoutMinutes": 5}}"#).unwrap();
        let c = load_config(&p);
        assert_eq!(c["runner"]["timeoutMinutes"], json!(5));
        assert_eq!(c["runner"]["permissionMode"], json!("acceptEdits"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
