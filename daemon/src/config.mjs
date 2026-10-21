// Where the daemon keeps its things, and its settings file.
//
// Everything lives under ~/.wheelhouse/daemon (WHEELHOUSE_DAEMON_HOME overrides it, for tests):
//   config.json      the pairing (server, device id, the server's pinned key) and local settings; mode 0600
//   repos/           one bare clone per repository, made on its first job
//   worktrees/       one worktree per job
//   state/jobs/      a record per job in progress, so a restart can clean up after one it lost
//   logs/            what launchd writes

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export const daemonHome = () => process.env.WHEELHOUSE_DAEMON_HOME || path.join(os.homedir(), ".wheelhouse", "daemon");
export const paths = (home = daemonHome()) => ({
  home,
  config: path.join(home, "config.json"),
  repos: path.join(home, "repos"),
  worktrees: path.join(home, "worktrees"),
  jobs: path.join(home, "state", "jobs"),
  logs: path.join(home, "logs"),
});

/** What a run may do without asking. There is nobody to ask, so anything else is refused, and the agent carries on without it. */
export const DEFAULT_RUNNER = {
  // The built-in tools the agent has at all; --restricted removes the rest.
  tools: ["Read", "Edit", "Write", "Glob", "Grep", "Bash", "TodoWrite"],
  permissionMode: "acceptEdits",
  allowedTools: [
    "Bash(git status:*)",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(git add:*)",
    "Bash(git commit:*)",
    "Bash(ls:*)",
    "Bash(npm test:*)",
    "Bash(npm run:*)",
    "Bash(npm install:*)",
    "Bash(npx tsc:*)",
    "Bash(pnpm test:*)",
    "Bash(pnpm run:*)",
    "Bash(pnpm install:*)",
    "Bash(pnpm typecheck:*)",
    "Bash(pnpm lint:*)",
    "Bash(yarn test:*)",
    "Bash(node --test:*)",
  ],
  // The daemon pushes the branch itself, after the run.
  disallowedTools: ["Bash(git push:*)", "Bash(git checkout:*)", "Bash(git switch:*)", "Bash(git reset:*)", "Bash(git worktree:*)"],
  timeoutMinutes: 45,
};

export const DEFAULT_CONFIG = {
  // Set by `pair`.
  server: null,
  deviceId: null,
  serverPublicKey: null,
  workspace: null,
  name: null,
  // Set from the server's settings messages.
  poolSize: 3,
  chatModel: null,
  // Where the stream resumes after a restart.
  lastEventId: null,
  // Local only: never sent anywhere.
  keystore: process.platform === "darwin" ? "keychain" : "file",
  claudePath: "claude",
  gitProtocol: "https", // or "ssh": clone git@github.com:owner/name.git instead
  runner: DEFAULT_RUNNER,
  // { "<repository id>": { "setup": "pnpm install" } }: run in each new worktree before the agent starts.
  repositories: {},
  // Finished worktrees that failed are kept this long for a look, then removed.
  keepFailedHours: 24,
};

export function loadConfig(p = paths()) {
  let saved = {};
  try {
    saved = JSON.parse(fs.readFileSync(p.config, "utf8"));
  } catch {
    /* no config yet */
  }
  return { ...DEFAULT_CONFIG, ...saved, runner: { ...DEFAULT_RUNNER, ...(saved.runner ?? {}) } };
}

/** Write the whole config atomically (a crash mid-write never leaves half a file), readable only by this user. */
export function saveConfig(config, p = paths()) {
  fs.mkdirSync(p.home, { recursive: true, mode: 0o700 });
  const tmp = `${p.config}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(config, null, 2) + "\n", { mode: 0o600 });
  fs.renameSync(tmp, p.config);
}

export function updateConfig(patch, p = paths()) {
  const next = { ...loadConfig(p), ...patch };
  saveConfig(next, p);
  return next;
}
