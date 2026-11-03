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
