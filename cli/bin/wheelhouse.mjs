#!/usr/bin/env node
// wheelhouse: CLI + Claude Code hook runner for Shipstack Wheelhouse.
//
// Zero dependencies (Node >= 18); ../lib holds what it shares with the daemon
// (terminal look, Keychain). Content hashes are git blob ids (`git hash-object`),
// which match committed blobs exactly.
//
// Every hook fails OPEN: if Wheelhouse is unreachable or anything throws, the
// agent's work continues and the error is logged to .wheelhouse/hook.log.

import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { keychainGet, keychainSet } from "../lib/keychain.mjs";
import { detectLevel, header, sym } from "../lib/ui.mjs";

const VERSION = "0.2.0";
const CONFIG_FILE = "wheelhouse.json"; // committed: { url, team, defaultBranch }
const STATE_DIR = ".wheelhouse"; // gitignored: context.md, sessions/, hook.log
const HOME_DIR = path.join(os.homedir(), ".wheelhouse"); // per user: credentials (off macOS), daemon/
const CRED_FILE = path.join(HOME_DIR, "credentials.json");
const USER_CONFIG = path.join(HOME_DIR, "config.json"); // default { url, team } for commands run outside a linked repo
const OLD_CRED_FILE = path.join(os.homedir(), ".config", "wheelhouse", "credentials.json"); // before 0.2.0
const EDIT_TOOLS = new Set(["Edit", "Write", "MultiEdit", "NotebookEdit"]);
const BASELINE_PENDING = "baseline-pending"; // marker in .wheelhouse/: init ran without a key, the plugin sends the baseline
const QUEUE_DIGEST = 10; // changes injected per prompt; the rest stay queued for get_changes

/** What to tell the agent when a teammate had removed its session and it reconnected (server: session_removed). */
function removalNotice(gone) {
  const when = gone.removedAt ? ` on ${new Date(gone.removedAt).toISOString().slice(0, 10)}` : "";
  const n = Number(gone.missedChanges) || 0;
  return `Wheelhouse: a teammate removed this session${when}. It has reconnected at the top of the queue, so ${n} change(s) made since you last read it were skipped. Ask the user whether to replay them or skip them, then call the Wheelhouse tool resume_session with mode "replay" or "skip".`;
}

// ───────── small utils ─────────

function git(args, opts = {}) {
  try {
    return execFileSync("git", args, { encoding: "utf8", stdio: ["pipe", "pipe", "ignore"], maxBuffer: 64 * 1024 * 1024, ...opts }).trimEnd();
  } catch {
    return null;
  }
}

function repoRoot(cwd = process.cwd()) {
  return git(["rev-parse", "--show-toplevel"], { cwd }) ?? cwd;
}

function readJson(file, fallback) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return fallback;
  }
}

function writeJson(file, data, mode) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(data, null, 2) + "\n", mode ? { mode } : undefined);
}

function log(root, msg) {
  try {
    fs.mkdirSync(path.join(root, STATE_DIR), { recursive: true });
    fs.appendFileSync(path.join(root, STATE_DIR, "hook.log"), `${new Date().toISOString()} ${msg}\n`);
  } catch {
    /* never fail on logging */
  }
}

async function readStdin() {
  if (process.stdin.isTTY) return {};
  let data = "";
  for await (const chunk of process.stdin) data += chunk;
  try {
    return data.trim() ? JSON.parse(data) : {};
  } catch {
    return {};
  }
}

function parseArgs(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      const [k, v] = a.slice(2).split("=");
      if (v !== undefined) out[k] = v;
      else if (argv[i + 1] && !argv[i + 1].startsWith("--")) out[k] = argv[++i];
      else out[k] = true;
    } else out._.push(a);
  }
  return out;
}

const rel = (root, p) => {
  if (!p) return null;
  const abs = path.isAbsolute(p) ? p : path.join(root, p);
  const r = path.relative(root, abs).split(path.sep).join("/");
  return r.startsWith("..") ? null : r;
};

// ───────── config + API ─────────

function loadConfig(root, flags = {}) {
  const file = readJson(path.join(root, CONFIG_FILE), {});
  // First that is set wins. Wheelhouse's own settings come first; the Claude Code plugin's options are a fallback.
  const user = readJson(USER_CONFIG, {});
  const [url, urlFrom] = pick([flags.url, "--url"], [process.env.WHEELHOUSE_URL, "WHEELHOUSE_URL"], [file.url, CONFIG_FILE], [user.url, USER_CONFIG], [process.env.CLAUDE_PLUGIN_OPTION_SERVER_URL, "plugin"]);
  const [key, keyFrom] = pick([flags.key, "--key"], [process.env.WHEELHOUSE_KEY, "WHEELHOUSE_KEY"], [() => url && loadKey(url), keyStoreName()], [process.env.CLAUDE_PLUGIN_OPTION_API_KEY, "plugin"]);
  if (file.project && !file.team) {
    console.error(`${CONFIG_FILE} uses the old \`project\` field. Wheelhouse now links a repo to a team: run \`wheelhouse init --team ${file.project}\` again.`);
    process.exit(1);
  }
  const [team, teamFrom] = pick([flags.team, "--team"], [process.env.WHEELHOUSE_TEAM, "WHEELHOUSE_TEAM"], [file.team, CONFIG_FILE], [user.team, USER_CONFIG]);
  const defaultBranch = file.defaultBranch || detectDefaultBranch(root) || "main";
  return { url, key, team, defaultBranch, from: { url: urlFrom, key: keyFrom, team: teamFrom }, configured: !!(url && key && team) };
}

/** The first source that is set, as [value, where it came from]. A function is only called when reached. */
function pick(...sources) {
  for (const [v, from] of sources) {
    const value = typeof v === "function" ? v() : v;
    if (typeof value === "string" && value) return [value.replace(/\/+$/, ""), from];
  }
  return ["", null];
}

// ───────── keys ─────────
// macOS: the login Keychain, one item per server URL. Elsewhere (and with WHEELHOUSE_KEYSTORE=file, which tests use):
// ~/.wheelhouse/credentials.json, readable only by this user. Keys saved before 0.2.0 move over on first use.

const KEYCHAIN_SERVICE = "com.shipstack.wheelhouse";
const useKeychain = () => (process.env.WHEELHOUSE_KEYSTORE || (process.platform === "darwin" ? "keychain" : "file")) === "keychain";
const keyStoreName = () => (useKeychain() ? "Keychain" : CRED_FILE);

function loadKey(url) {
  migrateOldCredentials();
  return (useKeychain() ? keychainGet(KEYCHAIN_SERVICE, url) : readJson(CRED_FILE, {})[url]) || "";
}

/** Save a key for a server; chmod too, because the mode on writeFile only applies when the file is created. */
function saveCredential(url, key) {
  if (useKeychain()) return keychainSet(KEYCHAIN_SERVICE, url, `Wheelhouse (${url})`, key);
  const creds = readJson(CRED_FILE, {});
  creds[url] = key;
  writeJson(CRED_FILE, creds, 0o600);
  try {
    fs.chmodSync(CRED_FILE, 0o600);
  } catch {
    /* best effort */
  }
}

/** Remember a default server and team for commands run outside a linked repo. `only` keeps what is already set. */
function rememberDefault(values, { only = false } = {}) {
  const cur = readJson(USER_CONFIG, {});
  const next = only ? { ...values, ...cur } : { ...cur, ...values };
  if (JSON.stringify(next) !== JSON.stringify(cur)) writeJson(USER_CONFIG, next);
}

function migrateOldCredentials() {
  if (!fs.existsSync(OLD_CRED_FILE)) return;
  for (const [url, key] of Object.entries(readJson(OLD_CRED_FILE, {}))) saveCredential(url, key);
  fs.rmSync(OLD_CRED_FILE);
}

function detectDefaultBranch(root) {
  const ref = git(["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"], { cwd: root });
  if (ref) return ref.replace(/^origin\//, "");
  for (const b of ["main", "master"]) if (git(["rev-parse", "--verify", "--quiet", b], { cwd: root })) return b;
  return null;
}

async function call(cfg, tool, args, timeoutMs = 4000) {
  const ctl = new AbortController();
  const t = setTimeout(() => ctl.abort(), timeoutMs);
  try {
    const res = await fetch(`${cfg.url}/api/v1/${tool}`, {
      method: "POST",
      headers: { Authorization: `Bearer ${cfg.key}`, "Content-Type": "application/json", "User-Agent": `wheelhouse-cli/${VERSION}` },
      body: JSON.stringify({ ...(cfg.team ? { team: cfg.team } : {}), ...args }),
      signal: ctl.signal,
    });
    const body = await res.json().catch(() => ({}));
    if (res.status === 404 && /Unknown tool/.test(body?.error?.message ?? "")) {
      throw Object.assign(new Error(`${tool}: the server does not know this tool. Update the Wheelhouse CLI/plugin (or the server) so they match.`), { status: 404 });
    }
    if (!res.ok) throw Object.assign(new Error(`${tool}: ${res.status} ${body?.error?.message ?? ""}`.trim()), { status: res.status });
    return body.data;
  } finally {
    clearTimeout(t);
  }
}

// ───────── interactive login ─────────

const DEFAULT_URL = "https://wheelhouse.shipstack.run"; // override with --url, WHEELHOUSE_URL or wheelhouse.json (local dev: http://localhost:3456)
const LOGIN_TRIES = 3;

const canPrompt = () => !!(process.stdin.isTTY && process.stdout.isTTY) && !process.env.CI;


/** Read one line from the terminal. With `hidden`, nothing is echoed (not even asterisks), so the key never reaches the screen or scrollback. */
function prompt(question, { hidden = false, fallback = "" } = {}) {
  return new Promise((resolve, reject) => {
    const stdin = process.stdin;
    const label = fallback && !hidden ? `${question} [${fallback}]: ` : `${question}: `;
    process.stdout.write(label);
    let buf = "";
    stdin.setRawMode(true);
    stdin.resume();
    stdin.setEncoding("utf8");
    const done = (fn, v) => {
      stdin.removeListener("data", onData);
      stdin.setRawMode(false);
      stdin.pause();
      process.stdout.write("\n");
      fn(v);
    };
    const onData = (chunk) => {
      for (const ch of chunk) {
        if (ch === "\r" || ch === "\n") return done(resolve, buf.trim() || fallback);
        if (ch === "\u0003" || ch === "\u0004") return done(reject, new Error("cancelled")); // Ctrl-C / Ctrl-D
        if (ch === "\u007f" || ch === "\b") {
          if (buf && !hidden) process.stdout.write("\b \b");
          buf = buf.slice(0, -1);
        } else if (ch >= " ") {
          buf += ch;
          if (!hidden) process.stdout.write(ch);
        }
      }
    };
    stdin.on("data", onData);
  });
}

/** Ask the server whether a key works. Returns "ok", "rejected" (401/403) or "unreachable". */
async function checkKey(url, key) {
  try {
    await call({ url, key }, "get_briefing", {}, 8000);
    return { state: "ok" };
  } catch (e) {
    if (e.status === 401 || e.status === 403) return { state: "rejected" };
    return { state: "unreachable", message: e.name === "AbortError" ? "timed out" : e.message };
  }
}

/**
 * Prompt for the server URL and API key (hidden), validate against the server, and save on success.
 * Returns the saved { url, key }, or null when the user gave up / ran out of tries.
 */
async function interactiveLogin(root, flags = {}) {
  const known = loadConfig(root, flags).url;
  const url = (flags.url ? String(flags.url) : await prompt("Wheelhouse URL", { fallback: known || DEFAULT_URL })).replace(/\/+$/, "");
  for (let attempt = 1; attempt <= LOGIN_TRIES; attempt++) {
    const key = await prompt("API key (input hidden)", { hidden: true });
    if (!key) {
      console.error("No key entered.");
    } else {
      const r = await checkKey(url, key);
      if (r.state === "ok") {
        saveCredential(url, key);
        rememberDefault({ url });
        console.log(`✓ logged in to ${url}; key saved to ${keyStoreName()}`);
        return { url, key };
      }
      if (r.state === "unreachable") {
        console.error(`Could not reach ${url} (${r.message}). Check the URL and that the server is running.`);
        return null;
      }
      console.error(`${url} rejected that key (401). Create one under Agents in the web app.${attempt < LOGIN_TRIES ? ` Try again (${attempt}/${LOGIN_TRIES}).` : ""}`);
    }
  }
  console.error("Giving up after 3 tries.");
  return null;
}

// ───────── git facts ─────────

/** Content hashes (git blob ids) for working-tree files; null for missing/deleted. */
function hashFiles(root, files) {
  const out = {};
  const present = files.filter((f) => fs.existsSync(path.join(root, f)) && fs.statSync(path.join(root, f)).isFile());
  for (const f of files) out[f] = null;
  if (present.length) {
    const res = git(["hash-object", "--stdin-paths"], { cwd: root, input: present.join("\n") + "\n" });
    if (res) res.split("\n").forEach((h, i) => (out[present[i]] = h.trim()));
  }
  return out;
}

/** Dirty paths (modified, added, deleted, untracked, renamed) relative to the repo root. */
function dirtyPaths(root) {
  const out = git(["status", "--porcelain=v1", "-z", "--untracked-files=all"], { cwd: root });
  if (!out) return [];
  const parts = out.split("\0").filter(Boolean);
  const paths = [];
  for (let i = 0; i < parts.length; i++) {
    const code = parts[i].slice(0, 2);
    paths.push(parts[i].slice(3));
    if (code.includes("R") || code.includes("C")) i++; // skip the rename source
  }
  return paths;
}

function snapshot(root) {
  const files = dirtyPaths(root);
  return hashFiles(root, files);
}

function diffSnapshots(before, after) {
  const changed = [];
  for (const [f, h] of Object.entries(after)) if (before[f] !== h) changed.push(f);
  for (const f of Object.keys(before)) if (!(f in after)) changed.push(f); // now clean (e.g. reverted or committed)
  return [...new Set(changed)];
}

function branch(root) {
  return git(["rev-parse", "--abbrev-ref", "HEAD"], { cwd: root }) ?? undefined;
}

// ───────── local session state ─────────

const sessionFile = (root, id) => path.join(root, STATE_DIR, "sessions", `${String(id).replace(/[^\w.-]/g, "_")}.json`);
const loadSession = (root, id) => readJson(sessionFile(root, id), { files: [], blocks: {}, lastSnapshot: null, lastQueueCheck: 0 });
const saveSession = (root, id, s) => writeJson(sessionFile(root, id), s);

async function reportTouched(cfg, root, sessionId, files, external = false) {
  const s = loadSession(root, sessionId);
  if (!external) s.files = [...new Set([...s.files, ...files])];
  saveSession(root, sessionId, s);
  await call(cfg, "session_sync", { session: sessionId, client: "claude-code", branch: branch(root), files: hashFiles(root, files), external });
}

// ───────── landing outbox ─────────
// Git hooks run outside Claude Code and cannot read the plugin's keychain key, so they only queue what
// landed (a JSON file per landing in .wheelhouse/outbox). Anything that has the key flushes the queue:
// the plugin's session-start and stop hooks, or a git hook itself when a key is configured for the CLI.

const outboxDir = (root) => path.join(root, STATE_DIR, "outbox");

/** Queue one landing. Never throws: a failed write must not disturb a commit. */
function queueLanding(root, entry) {
  try {
    const dir = outboxDir(root);
    fs.mkdirSync(dir, { recursive: true });
    const name = `${Date.now()}-${String(entry.sha ?? "unknown").slice(0, 12)}.json`;
    const tmp = path.join(dir, `.${name}.tmp`);
    fs.writeFileSync(tmp, JSON.stringify(entry) + "\n");
    fs.renameSync(tmp, path.join(dir, name));
    return true;
  } catch (e) {
    log(root, `queue landing: ${e.message}`);
    return false;
  }
}

function pendingLandings(root) {
  try {
    return fs.readdirSync(outboxDir(root)).filter((n) => n.endsWith(".json") && !n.startsWith(".")).sort();
  } catch {
    return [];
  }
}

/**
 * Send queued landings to the server, oldest first, deleting each once it is accepted. Stops at the first
 * failure (offline, bad key, server error) and leaves the rest queued. The server treats a repeated landing as
 * a no-op, so two flushers racing is harmless. Returns how many were sent.
 */
async function flushOutbox(cfg, root, session) {
  let sent = 0;
  for (const name of pendingLandings(root)) {
    const file = path.join(outboxDir(root), name);
    const e = readJson(file, null);
    if (!e || typeof e.files !== "object") {
      fs.rmSync(file, { force: true }); // unreadable entry: nothing to retry
      continue;
    }
    try {
      await call(cfg, "land_changes", { sha: e.sha, branch: e.branch, files: e.files, ...(session ? { session } : {}) }, 15000);
    } catch (err) {
      if (err.status === 400 || err.status === 422) {
        log(root, `outbox: dropped ${name}: ${err.message}`); // the server will never accept this payload
        fs.rmSync(file, { force: true });
        continue;
      }
      log(root, `outbox: stopped at ${name}: ${err.message}`);
      break;
    }
    fs.rmSync(file, { force: true });
    sent++;
    if (e.sha && e.sha === git(["rev-parse", "HEAD"], { cwd: root })) writeJson(path.join(root, STATE_DIR, "landed.json"), { sha: e.sha });
  }
  return sent;
}

/**
 * Report commits that reached the default branch since we last looked. Lets
 * Claude Code sessions land work even where git hooks aren't installed (or
 * can't reach the API key). Idempotent: the server treats repeats as no-ops.
 */
async function autoLand(cfg, root, session) {
  if (branch(root) !== cfg.defaultBranch) return;
  const head = git(["rev-parse", "HEAD"], { cwd: root });
  if (!head) return;
  const stateFile = path.join(root, STATE_DIR, "landed.json");
  const state = readJson(stateFile, {});
  if (state.sha === head) return;
  const known = state.sha && git(["merge-base", "--is-ancestor", state.sha, head], { cwd: root }) !== null;
  if (!state.sha || !known) {
    writeJson(stateFile, { sha: head }); // first sight (or history rewritten): start tracking from here
    return;
  }
  await landRange(cfg, root, state.sha, head, session);
  writeJson(stateFile, { sha: head });
}

async function landRange(cfg, root, from, to, session) {
  const names = git(["diff", "--name-only", "--no-renames", from, to], { cwd: root });
  const changed = (names ?? "").split("\n").filter(Boolean);
  if (!changed.length) return null;
  const files = Object.fromEntries(changed.map((f) => [f, null]));
  const tree = git(["ls-tree", "-r", "--full-tree", to, "--", ...changed], { cwd: root }) ?? "";
  for (const line of tree.split("\n")) {
    const m = /^\d+ blob ([0-9a-f]+)\t(.+)$/.exec(line);
    if (m) files[m[2]] = m[1];
  }
  const sha = git(["rev-parse", to], { cwd: root }) ?? undefined;
  return call(cfg, "land_changes", { sha, branch: cfg.defaultBranch, files, ...(session ? { session } : {}) }, 15000);
}

// ───────── hook handlers (Claude Code) ─────────

const hooks = {
  /** SessionStart: register the session, write .wheelhouse/context.md, inject the briefing. */
  async "session-start"(cfg, root, input) {
    const id = input.session_id;
    const s = loadSession(root, id);
    s.lastSnapshot = snapshot(root);
    saveSession(root, id, s);
    if (fs.existsSync(path.join(root, STATE_DIR, BASELINE_PENDING))) {
      await cmdBaseline(cfg, root, { quiet: true }).catch((e) => log(root, `session-start baseline: ${e.message}`));
    }
    await flushOutbox(cfg, root, id).catch((e) => log(root, `session-start flush: ${e.message}`));
    await autoLand(cfg, root, id).catch((e) => log(root, `session-start land: ${e.message}`));
    await call(cfg, "session_sync", { session: id, client: "claude-code", branch: branch(root), files: [] }).catch((e) => log(root, `session-start sync: ${e.message}`));
    // With `session`, the briefing registers this session's own queue cursor and tells the agent its id.
    const b = await call(cfg, "get_briefing", { max_chars: 6000, session: id }, 6000);
    const gone = b.session_removed;
    if (gone) {
      s.notifiedRemoval = String(gone.removedAt);
      saveSession(root, id, s);
    }
    const head = git(["rev-parse", "--short", "HEAD"], { cwd: root }) ?? "unknown";
    const md = `<!-- generated by wheelhouse ${VERSION} at ${new Date().toISOString()} for ${head}; do not edit -->\n${b.markdown}\n`;
    fs.mkdirSync(path.join(root, STATE_DIR), { recursive: true });
    fs.writeFileSync(path.join(root, STATE_DIR, "context.md"), md);
    return {
      hookSpecificOutput: {
        hookEventName: "SessionStart",
        additionalContext: `${gone ? removalNotice(gone) + "\n\n" : ""}${b.markdown}\n\n(Wheelhouse team ${cfg.team}: pass team "${cfg.team}" on every Wheelhouse tool call. This briefing is also saved at ${STATE_DIR}/context.md. Before working an issue call get_issue. The board and map are kept in sync by hooks: changed files are tracked automatically, and before you finish a turn you'll be asked to update the issue and map if needed.)`,
      },
    };
  },

  /** PostToolUse (Edit|Write|MultiEdit|NotebookEdit): record the touched file. Runs async. */
  async "post-edit"(cfg, root, input) {
    if (input.tool_name && !EDIT_TOOLS.has(input.tool_name)) return;
    const ti = input.tool_input ?? {};
    const f = rel(root, ti.file_path ?? ti.notebook_path ?? input.tool_response?.filePath);
    if (!f || f.startsWith(`${STATE_DIR}/`)) return;
    await reportTouched(cfg, root, input.session_id, [f]);
  },

  /**
   * PreToolUse (Wheelhouse MCP tools): tag the call with this Claude Code session.
   * The model must never be trusted to supply the id (it may omit, mistype or invent it),
   * so whatever it passed is overwritten. updatedInput replaces the whole input, hence the
   * spread. No permissionDecision on purpose: this must not auto-approve Wheelhouse tools
   * the user's permission rules would otherwise prompt for. No network call, so it's instant.
   */
  async "pre-mcp"(cfg, root, input) {
    const ti = input.tool_input;
    if (!input.session_id || !ti || typeof ti !== "object" || Array.isArray(ti)) return; // fail open: server falls back to the key-level cursor
    return { hookSpecificOutput: { hookEventName: "PreToolUse", updatedInput: { ...ti, session: input.session_id } } };
  },

  /** PreToolUse (Bash): snapshot the dirty working tree so post-bash can see what the command changed. */
  async "pre-bash"(cfg, root, input) {
    const snapFile = path.join(root, STATE_DIR, "bash", `${input.tool_use_id ?? "last"}.json`);
    writeJson(snapFile, snapshot(root));
  },

  /** PostToolUse (Bash): files the command changed (codegen, sed, migrations, package installs…). */
  async "post-bash"(cfg, root, input) {
    const snapFile = path.join(root, STATE_DIR, "bash", `${input.tool_use_id ?? "last"}.json`);
    const before = readJson(snapFile, null);
    try {
      fs.rmSync(snapFile, { force: true });
    } catch {}
    if (!before) return;
    const changed = diffSnapshots(before, snapshot(root)).filter((f) => !f.startsWith(`${STATE_DIR}/`));
    if (changed.length) await reportTouched(cfg, root, input.session_id, changed);
  },

  /**
   * UserPromptSubmit: catch edits made outside the agent, and deliver new team
   * changes from this session's own queue. What's shown is acked for this
   * session only, so other sessions on the same key still get it.
   */
  async prompt(cfg, root, input) {
    const id = input.session_id;
    const s = loadSession(root, id);
    const now = snapshot(root);
    if (s.lastSnapshot) {
      const external = diffSnapshots(s.lastSnapshot, now).filter((f) => !s.files.includes(f) && !f.startsWith(`${STATE_DIR}/`));
      if (external.length) await reportTouched(cfg, root, id, external, true).catch((e) => log(root, `prompt external: ${e.message}`));
    }
    s.lastSnapshot = now;
    let context = null;
    if (Date.now() - (s.lastQueueCheck ?? 0) > 60_000) {
      s.lastQueueCheck = Date.now();
      const q = await call(cfg, "get_changes", { limit: QUEUE_DIGEST, session: id }, 3000).catch(() => null);
      // Tell the agent once per removal (the server keeps reporting it until the agent chooses).
      if (q?.session_removed && s.notifiedRemoval !== String(q.session_removed.removedAt)) {
        s.notifiedRemoval = String(q.session_removed.removedAt);
        context = removalNotice(q.session_removed);
      }
      if (q?.total) {
        const shown = q.changes;
        const lines = shown.map((c) => `- ${c.actorName} ${c.summary}`).join("\n");
        const more = q.total - shown.length;
        context = `${context ? context + "\n\n" : ""}Wheelhouse: ${q.total} new change(s) from the team in ${cfg.team} since you last checked:\n${lines}${
          more > 0 ? `\n…and ${more} more. Call get_changes to see them.` : ""
        }\nHandle anything addressed to you (these are already marked seen for this session).`;
        const last = shown.at(-1)?.seq;
        if (last) await call(cfg, "get_changes", { ack: last, limit: 1, session: id }, 3000).catch((e) => log(root, `prompt ack: ${e.message}`));
      }
    }
    saveSession(root, id, s);
    return context ? { hookSpecificOutput: { hookEventName: "UserPromptSubmit", additionalContext: context } } : undefined;
  },

  /** Stop: refresh hashes, then block the turn (at most twice per prompt) while the session owes updates. */
  async stop(cfg, root, input) {
    const id = input.session_id;
    const s = loadSession(root, id);
    await flushOutbox(cfg, root, id).catch((e) => log(root, `stop flush: ${e.message}`));
    await autoLand(cfg, root, id).catch((e) => log(root, `stop land: ${e.message}`));
    if (s.files.length) {
      await call(cfg, "session_sync", { session: id, client: "claude-code", branch: branch(root), files: hashFiles(root, s.files) }, 6000);
    }
    const status = await call(cfg, "session_status", { session: id }, 6000);
    if (status.clean) {
      s.files = status.files ?? [];
      saveSession(root, id, s);
      return;
    }
    const key = input.prompt_id ?? "turn";
    s.blocks = { [key]: (s.blocks?.[key] ?? 0) + 1 }; // only track the current prompt
    saveSession(root, id, s);
    if (s.blocks[key] > 2) {
      // Don't trap the agent: after two reminders, let the turn end and tell the human.
      return { systemMessage: `Wheelhouse: this session still has unsynced work in ${cfg.team} (see the issue/map). It will be flagged on the board.` };
    }
    return { decision: "block", reason: status.message };
  },

  /** SessionEnd: mark the session ended (tight time budget; best effort). */
  async "session-end"(cfg, root, input) {
    await call(cfg, "session_end", { session: input.session_id }, 1200);
  },
};

async function runHook(event) {
  const input = await readStdin();
  const root = repoRoot(input.cwd || process.env.CLAUDE_PROJECT_DIR || process.cwd());
  const cfg = loadConfig(root);
  const handler = hooks[event];
  if (!handler) {
    log(root, `unknown hook event ${event}`);
    return;
  }
  if (!cfg.configured) return; // repo not linked to Wheelhouse: stay silent
  try {
    const out = await handler(cfg, root, input);
    if (out) process.stdout.write(JSON.stringify(out));
  } catch (e) {
    log(root, `${event}: ${e?.message ?? e}`); // fail open
  }
}

// ───────── commands ─────────

async function cmdInit(flags) {
  const root = repoRoot();
  const existing = readJson(path.join(root, CONFIG_FILE), {});
  const url = (flags.url || existing.url || process.env.WHEELHOUSE_URL || DEFAULT_URL).replace(/\/+$/, "");
  const team = (flags.team || existing.team || "").toUpperCase();
  if (!team) {
    console.error(`Usage: wheelhouse init --team <KEY> [--url <https://wheelhouse.example>] [--key wh_...] [--no-git-hooks] [--no-baseline]\nThe URL defaults to ${DEFAULT_URL}.`);
    process.exit(1);
  }
  const defaultBranch = flags.branch || existing.defaultBranch || detectDefaultBranch(root) || "main";
  writeJson(path.join(root, CONFIG_FILE), { url, team, defaultBranch });
  rememberDefault({ url, team }, { only: true });
  console.log(`✓ wrote ${CONFIG_FILE} (url ${url}, team ${team}, default branch ${defaultBranch})`);

  if (flags.key) {
    saveCredential(url, String(flags.key));
    console.log(`✓ saved API key for ${url} to ${keyStoreName()}`);
  }

  // .gitignore
  const gi = path.join(root, ".gitignore");
  const cur = fs.existsSync(gi) ? fs.readFileSync(gi, "utf8") : "";
  if (!cur.split("\n").some((l) => l.trim() === `${STATE_DIR}/` || l.trim() === STATE_DIR)) {
    fs.writeFileSync(gi, cur + (cur.endsWith("\n") || !cur ? "" : "\n") + `${STATE_DIR}/\n`);
    console.log(`✓ added ${STATE_DIR}/ to .gitignore`);
  }

  if (!flags["no-git-hooks"]) installGitHooks(root);

  let cfg = loadConfig(root, flags);
  // A stored key can be stale (revoked, other server): in a terminal, offer to log in again instead of failing later in a hook.
  if (canPrompt() && (!cfg.key || (await checkKey(cfg.url, cfg.key)).state === "rejected")) {
    console.log(cfg.key ? "The saved API key was rejected. Let's log in again." : "No API key yet. Let's log in.");
    await interactiveLogin(root, { ...flags, url }).catch(() => null);
    cfg = loadConfig(root, { ...flags, key: undefined });
  }
  if (!cfg.key) {
    // No key on this machine is fine for Claude Code users: git hooks only queue landings, and the plugin (which has
    // the key) sends them and the baseline at the next session start.
    if (!flags["no-baseline"]) {
      fs.mkdirSync(path.join(root, STATE_DIR), { recursive: true });
      fs.writeFileSync(path.join(root, STATE_DIR, BASELINE_PENDING), new Date().toISOString() + "\n");
    }
    console.error(
      "! No API key on this machine, so nothing was sent to the server yet.\n" +
        "  Git hooks will queue commits locally. Open a Claude Code session with the Wheelhouse plugin and it sends them,\n" +
        "  along with the baseline. To send from the terminal instead: `wheelhouse login` (or set WHEELHOUSE_KEY), then `wheelhouse land --flush`.",
    );
    return;
  }
  try {
    const { teams } = await call({ ...cfg, team: "" }, "get_briefing", {});
    if (!teams.some((t) => t.key === team)) console.log(`! team ${team} not found on ${url}; create it in the web app.`);
    else console.log(`✓ connected to ${url} as an agent key; team ${team} found`);
  } catch (e) {
    console.log(`! could not reach Wheelhouse: ${e.message}`);
    return;
  }
  if (!flags["no-baseline"]) await cmdBaseline(cfg, root);
  console.log(`\nNext: install the Claude Code plugin (see the README) so sessions are synced automatically.`);
}

function installGitHooks(root) {
  const hooksDir = git(["rev-parse", "--git-path", "hooks"], { cwd: root });
  if (!hooksDir) return console.log("! not a git repository; skipped git hooks");
  const dir = path.isAbsolute(hooksDir) ? hooksDir : path.join(root, hooksDir);
  fs.mkdirSync(dir, { recursive: true });
  const self = path.resolve(process.argv[1]);
  const block = (event) =>
    `# >>> wheelhouse (managed; remove this block to uninstall)\nif command -v node >/dev/null 2>&1 && [ -f "${self}" ]; then\n  (node "${self}" land --git-hook ${event} >/dev/null 2>&1 &)\nelif command -v wheelhouse >/dev/null 2>&1; then\n  (wheelhouse land --git-hook ${event} >/dev/null 2>&1 &)\nfi\n# <<< wheelhouse\n`;
  for (const event of ["post-commit", "post-merge", "post-rewrite"]) {
    const file = path.join(dir, event);
    let body = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "#!/bin/sh\n";
    body = body.replace(/# >>> wheelhouse[\s\S]*?# <<< wheelhouse\n?/g, "");
    if (!body.endsWith("\n")) body += "\n";
    fs.writeFileSync(file, body + block(event), { mode: 0o755 });
    fs.chmodSync(file, 0o755);
  }
  console.log(`✓ installed git hooks (post-commit, post-merge, post-rewrite) in ${path.relative(root, dir) || dir}`);
}

/** Report every tracked file on the default branch as the described baseline. */
async function cmdBaseline(cfg, root, { quiet = false } = {}) {
  const say = (m) => quiet || console.log(m); // a hook's stdout must stay clean
  const tree = git(["ls-tree", "-r", "--full-tree", cfg.defaultBranch], { cwd: root }) ?? git(["ls-tree", "-r", "--full-tree", "HEAD"], { cwd: root });
  if (!tree) return say("! no commits yet; skipped baseline");
  const files = {};
  for (const line of tree.split("\n")) {
    const m = /^\d+ blob ([0-9a-f]+)\t(.+)$/.exec(line);
    if (m) files[m[2]] = m[1];
  }
  const entries = Object.entries(files);
  for (let i = 0; i < entries.length; i += 2000) {
    await call(cfg, "land_changes", { baseline: true, branch: cfg.defaultBranch, files: Object.fromEntries(entries.slice(i, i + 2000)) }, 30000);
  }
  const head = git(["rev-parse", cfg.defaultBranch], { cwd: root });
  if (head) writeJson(path.join(root, STATE_DIR, "landed.json"), { sha: head });
  fs.rmSync(path.join(root, STATE_DIR, BASELINE_PENDING), { force: true });
  say(`✓ recorded baseline hashes for ${entries.length} files on ${cfg.defaultBranch}`);
}

/** Report what landed on the default branch (git hooks / CI). */
async function cmdLand(flags) {
  const root = repoRoot();
  const cfg = loadConfig(root, flags);
  const hook = !!flags["git-hook"];
  // Queueing needs only the link (wheelhouse.json), not a key: a git hook must work for someone who set up the plugin only.
  if (!cfg.url || !cfg.team) {
    if (!hook) console.error("wheelhouse is not configured here (run `wheelhouse init`).");
    return;
  }
  if (flags.flush) {
    if (!cfg.key) return console.error("No API key to send the queue with (run `wheelhouse login`, or set WHEELHOUSE_KEY).");
    const left = pendingLandings(root).length;
    const sent = await flushOutbox(cfg, root);
    return console.log(left ? `✓ sent ${sent} of ${left} queued landing(s)` : "Nothing queued.");
  }
  if (flags.baseline) {
    if (!cfg.key) return console.error("No API key (run `wheelhouse login`, or set WHEELHOUSE_KEY).");
    return cmdBaseline(cfg, root);
  }
  const current = branch(root);
  if (current !== cfg.defaultBranch && !flags.force) {
    if (!hook) console.log(`On ${current}, not ${cfg.defaultBranch}: nothing has landed. (Use --force to report anyway.)`);
    return;
  }
  const from = flags.from ?? (flags["git-hook"] === "post-merge" ? "ORIG_HEAD" : "HEAD~1");
  const to = flags.to ?? "HEAD";
  const range = git(["rev-parse", "--verify", "--quiet", from], { cwd: root }) ? [from, to] : null;
  const names = range
    ? git(["diff", "--name-only", "--no-renames", range[0], range[1]], { cwd: root })
    : git(["ls-tree", "-r", "--name-only", "--full-tree", to], { cwd: root }); // first commit
  const changed = (names ?? "").split("\n").filter(Boolean);
  if (!changed.length) return;
  const files = {};
  const tree = git(["ls-tree", "-r", "--full-tree", to, "--", ...changed], { cwd: root }) ?? "";
  for (const f of changed) files[f] = null;
  for (const line of tree.split("\n")) {
    const m = /^\d+ blob ([0-9a-f]+)\t(.+)$/.exec(line);
    if (m) files[m[2]] = m[1];
  }
  const sha = git(["rev-parse", to], { cwd: root }) ?? undefined;

  if (hook) {
    // Git hook: queue, never call the server. If this machine also has a CLI key (credentials file, env), send right away.
    queueLanding(root, { sha, branch: current, files, event: flags["git-hook"], queuedAt: new Date().toISOString() });
    if (cfg.key) await flushOutbox(cfg, root).catch((e) => log(root, `hook flush: ${e.message}`));
    return;
  }

  if (!cfg.key) return console.error("No API key (run `wheelhouse login`, or set WHEELHOUSE_KEY).");
  try {
    const r = await call(cfg, "land_changes", { sha, branch: current, files }, 15000);
    if (to === "HEAD" && sha) writeJson(path.join(root, STATE_DIR, "landed.json"), { sha });
    console.log(`✓ landed ${r.files} file(s)${r.published.length ? `; published drafts: ${r.published.join(", ")}` : ""}${r.stale.length ? `; drifted: ${r.stale.join(", ")}` : ""}${r.fresh.length ? `; back in sync: ${r.fresh.join(", ")}` : ""}`);
  } catch (e) {
    log(root, `land: ${e.message}`);
    console.error(`! ${e.message}`);
  }
}

async function cmdContext(flags) {
  const root = repoRoot();
  const cfg = loadConfig(root, flags);
  if (!cfg.configured) return console.error("wheelhouse is not configured here (run `wheelhouse init`).");
  const ref = flags.issue ?? flags._[1];
  if (ref) {
    const r = await call(cfg, "get_issue", { ref }, 8000);
    const files = r.readFirst?.length ? `Read first: ${r.readFirst.join(", ")}\n\n` : "";
    return process.stdout.write(`${r.issue.ref} ${r.issue.title}\n\n${files}${r.context}\n`);
  }
  const r = await call(cfg, "get_briefing", {}, 8000);
  fs.mkdirSync(path.join(root, STATE_DIR), { recursive: true });
  fs.writeFileSync(path.join(root, STATE_DIR, "context.md"), r.markdown + "\n");
  process.stdout.write(r.markdown + "\n");
}

// ───────── board ─────────
// The same REST tools as MCP (one write path). They work in any folder: the team comes from the ref, the repo, or ~/.wheelhouse.

function boardConfig(flags, ref) {
  const cfg = loadConfig(repoRoot(), flags);
  if (ref && /^[A-Za-z]+-\d+$/.test(ref)) cfg.team = ref.split("-")[0].toUpperCase();
  const fix = !cfg.url ? "run `wheelhouse login`, or pass --url" : !cfg.key ? `run \`wheelhouse login --url ${cfg.url}\`` : !cfg.team ? "pass --team KEY, or run inside a linked repo" : null;
  if (fix) {
    console.error(`${sym("fail", detectLevel({ stream: process.stderr }))} Not set up for the board: ${fix}.`);
    process.exit(1);
  }
  return cfg;
}

function needRef(ref, usage) {
  if (ref) return ref.toUpperCase();
  console.error(`Usage: wheelhouse ${usage}`);
  process.exit(1);
}

/** Run a board call; on failure print the server's message and exit 1, so scripts can rely on the exit code. */
async function board(cfg, tool, args) {
  try {
    return await call(cfg, tool, args, 15000);
  } catch (e) {
    console.error(`${sym("fail", detectLevel({ stream: process.stderr }))} ${e.name === "AbortError" ? `${tool}: timed out` : e.message}`);
    process.exit(1);
  }
}

const issueRow = (i) => `${i.ref.padEnd(8)} ${String(i.status).padEnd(12)} ${String(i.priority ?? "").padEnd(7)} ${i.title}`;

async function cmdIssues(flags) {
  const cfg = boardConfig(flags);
  const args = { limit: Number(flags.limit) || 50 };
  if (flags.mine) args.assignee = "me";
  for (const k of ["status", "cycle", "project", "label", "priority", "text"]) if (typeof flags[k] === "string") args[k] = flags[k];
  const r = await board(cfg, "list_issues", args);
  if (flags.json) return console.log(JSON.stringify(r, null, 2));
  if (!r.issues.length) return console.log("No issues match.");
  for (const i of r.issues) console.log(issueRow(i));
  if (r.more) console.log(`… ${r.more} more (--limit, or narrow with --status, --cycle, --mine, --text)`);
}

async function cmdIssue(flags) {
  const ref = needRef(flags._[0], "issue WH-12");
  const r = await board(boardConfig(flags, ref), "get_issue", { ref, max_chars: 2000 });
  if (flags.json) return console.log(JSON.stringify(r, null, 2));
  const i = r.issue;
  console.log(`${i.ref}  ${i.title}`);
  console.log([i.status, i.priority, i.assignee && `@${i.assignee}`, i.cycle && `cycle ${i.cycle}`, i.project].filter(Boolean).join(" · "));
  if (i.body) console.log(`\n${i.body}`);
  for (const c of r.comments ?? []) console.log(`\n${c.by} · ${String(c.at).slice(0, 10)}\n${c.body}`);
}

/** Claim the issue, move it to In Progress, and switch to its branch when run in a git repo. */
async function cmdStart(flags) {
  const ref = needRef(flags._[0], "start WH-12");
  const cfg = boardConfig(flags, ref);
  const r = await board(cfg, "update_issue", { ref, status: "in_progress", assignee: "me" });
  console.log(`${sym("ok")} ${ref} is In Progress and yours`);
  const root = repoRoot();
  if (flags["no-branch"] || !git(["rev-parse", "--git-dir"], { cwd: root })) return;
  const slug = r.title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40).replace(/-$/, "");
  const name = `wh/${ref}-${slug}`;
  const exists = git(["rev-parse", "--verify", "--quiet", name], { cwd: root });
  try {
    execFileSync("git", exists ? ["switch", name] : ["switch", "-c", name], { cwd: root, stdio: ["ignore", "ignore", "pipe"] });
    console.log(`${sym("ok")} on branch ${name}${exists ? "" : " (new)"}`);
  } catch (e) {
    console.error(`${sym("warn")} could not switch to ${name}: ${String(e.stderr || e.message).trim()}`);
  }
}

async function cmdDone(flags) {
  const ref = needRef(flags._[0], "done WH-12");
  await board(boardConfig(flags, ref), "update_issue", { ref, status: "review" });
  console.log(`${sym("ok")} ${ref} is in review`);
}

async function cmdComment(flags) {
  const [ref, ...words] = flags._;
  const body = words.join(" ");
  if (!body) needRef(null, 'comment WH-12 "what changed"');
  await board(boardConfig(flags, ref), "comment", { ref: ref.toUpperCase(), body });
  console.log(`${sym("ok")} commented on ${ref.toUpperCase()}`);
}

async function cmdNew(flags) {
  const title = flags._.join(" ");
  if (!title) needRef(null, 'new "title" [--body TEXT] [--priority P] [--status S] [--cycle current]');
  const args = { title };
  for (const k of ["body", "priority", "status", "cycle", "project"]) if (typeof flags[k] === "string") args[k] = flags[k];
  const r = await board(boardConfig(flags), "create_issue", args);
  if (flags.json) return console.log(JSON.stringify(r, null, 2));
  console.log(`${sym("ok")} created ${r.ref}: ${r.title}`);
}

// ───────── daemon ─────────
// `wheelhouse daemon …` forwards to the daemon that sits next to this CLI (daemon/ in the same checkout). The standalone
// `wheelhouse-daemon` stays for servers. Until the daemon is packaged, only a checkout of the repo has one.

const DAEMON_HOME = process.env.WHEELHOUSE_DAEMON_HOME || path.join(HOME_DIR, "daemon"); // the daemon's own settings and logs
const DAEMON_DIR = path.resolve(path.dirname(fs.realpathSync(process.argv[1])), "..", "..", "daemon");

function cmdDaemon(flags) {
  const [sub = "help", ...rest] = process.argv.slice(3);
  if (!fs.existsSync(path.join(DAEMON_DIR, "src", "cli.mjs"))) {
    console.error(`${sym("fail", detectLevel({ stream: process.stderr }))} No daemon next to this CLI (${DAEMON_DIR}). It ships with the CLI once it is packaged; until then run it from a checkout of shipstackhq/wheelhouse-cli.`);
    process.exit(1);
  }
  const run = (cmd, args) => process.exit(spawnSync(cmd, args, { stdio: "inherit" }).status ?? 1);
  const entry = path.join(DAEMON_DIR, "src", "main.mjs");
  const platform = path.join(DAEMON_DIR, "platform");
  const dry = flags["dry-run"];
  if (sub === "install" || sub === "uninstall") {
    if (process.platform === "darwin") return run("sh", [path.join(platform, "macos", `${sub}.sh`), ...(sub === "install" ? ["--entry", entry] : []), ...(dry ? ["--dry-run"] : [])]);
    if (process.platform === "win32")
      return run("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(platform, "windows", `${sub}.ps1`), ...(sub === "install" ? ["-Entry", entry] : []), ...(dry ? ["-WhatIf"] : [])]);
    console.error(`No background service for ${process.platform} yet. Run it in the foreground: wheelhouse daemon run`);
    process.exit(1);
  }
  if (sub === "logs") {
    const files = ["out", "err"].map((k) => path.join(DAEMON_HOME, "logs", `wheelhouse-daemon.${k}.log`)).filter((f) => fs.existsSync(f));
    if (!files.length) return console.log("No logs yet: the background service writes them (wheelhouse daemon install).");
    const follow = rest.includes("-f") || rest.includes("--follow");
    const lines = Number(rest[rest.findIndex((a) => a === "-n" || a === "--lines") + 1]) || 50;
    return run("tail", [...(follow ? ["-f"] : []), "-n", String(lines), ...files]);
  }
  // pair, status, unpair, run, help: the daemon's own commands.
  run(process.execPath, [path.join(DAEMON_DIR, "src", "cli.mjs"), sub, ...rest]);
}

// ───────── integrations ─────────
// Commands for one agent tool live under `wheelhouse integrations <name>`, so the top level stays about Wheelhouse.

const claudePlugins = (file) => readJson(path.join(os.homedir(), ".claude", "plugins", file), {});

const INTEGRATIONS = {
  "claude-code": {
    about: "Claude Code plugin: hooks keep the board and map in sync with each session",
    installed: () => Object.keys(claudePlugins("installed_plugins.json").plugins || {}).some((k) => k.startsWith("wheelhouse@")),
    fix: "wheelhouse integrations claude-code install",
    commands: { hook: ([event]) => runHook(event), install: (_, flags) => installClaudeCode(flags) },
  },
};

/** Add the Shipstack marketplace and install the plugin through the `claude` CLI. The key is set in Claude Code, so it never shows in a process list. */
function installClaudeCode(flags) {
  const claude = (args) => spawnSync("claude", args, { stdio: "inherit" });
  if (spawnSync("claude", ["--version"], { stdio: "ignore" }).error) {
    console.error("Claude Code is not installed (no `claude` command). Install it first: https://claude.com/claude-code");
    process.exit(1);
  }
  if (INTEGRATIONS["claude-code"].installed()) return console.log("✓ the Wheelhouse plugin is already installed in Claude Code");
  if (!claudePlugins("known_marketplaces.json").shipstack && claude(["plugin", "marketplace", "add", String(flags.from || "shipstackhq/wheelhouse-cli")]).status !== 0) process.exit(1);
  const url = loadConfig(process.cwd(), flags).url || DEFAULT_URL;
  if (claude(["plugin", "install", "wheelhouse@shipstack", "--config", `server_url=${url}`]).status !== 0) process.exit(1);
  console.log(`✓ installed the Wheelhouse plugin for ${url}.\nLast step: run \`claude\`, then /plugin configure wheelhouse@shipstack and paste your API key.`);
}

async function cmdIntegrations(flags) {
  const [name, sub, ...args] = flags._;
  if (!name) {
    const list = Object.entries(INTEGRATIONS).map(([n, it]) => ({ name: n, installed: it.installed(), about: it.about }));
    if (flags.json) return console.log(JSON.stringify(list, null, 2));
    for (const it of list) console.log(`${sym(it.installed ? "ok" : "off")} ${it.name.padEnd(12)} ${it.about}`);
    return;
  }
  const it = INTEGRATIONS[name];
  if (!it) {
    console.error(`Unknown integration "${name}". Available: ${Object.keys(INTEGRATIONS).join(", ")}.`);
    process.exit(1);
  }
  const run = it.commands[sub];
  if (!run) {
    console.error(`Usage: wheelhouse integrations ${name} <${Object.keys(it.commands).join(" | ")}>`);
    process.exit(1);
  }
  return run(args, flags);
}

// ───────── doctor ─────────

async function cmdDoctor(flags) {
  let root = null;
  try {
    root = repoRoot();
  } catch {
    /* not in a repo */
  }
  const cfg = loadConfig(root || process.cwd(), flags);
  const checks = [];
  // ok: fine, fail: broken (with the fix), off: optional and not set up.
  const line = (state, what, fix) => checks.push({ state: { "✓": "ok", "✗": "fail", "·": "off" }[state], check: what, ...(fix ? { fix } : {}) });

  if (!cfg.url) line("✗", "server: not set", "run inside a linked repo, pass --url, or `wheelhouse init --team KEY`");
  else if (!cfg.key) line("✗", `key: none for ${cfg.url}`, `wheelhouse login --url ${cfg.url}`);
  else {
    const r = await checkKey(cfg.url, cfg.key);
    if (r.state === "ok") line("✓", `server ${cfg.url} accepts the key (${cfg.from.key})`);
    else if (r.state === "rejected") line("✗", `server ${cfg.url} rejects the key (${cfg.from.key})`, `wheelhouse login --url ${cfg.url}`);
    else line("✗", `server ${cfg.url} unreachable (${r.message})`, "check the URL, and that the server is running");
  }

  if (!root) line("·", "repo: not in a git repository");
  else if (!cfg.team) line("✗", `repo: ${root} is not linked to a team`, "wheelhouse init --team KEY");
  else {
    line("✓", `repo linked to team ${cfg.team} (${cfg.from.team})`);
    const hooksDir = git(["rev-parse", "--git-path", "hooks"], { cwd: root });
    const hook = hooksDir && path.resolve(root, hooksDir, "post-commit");
    const ok = hook && fs.existsSync(hook) && fs.readFileSync(hook, "utf8").includes("# >>> wheelhouse");
    line(ok ? "✓" : "✗", `git hooks ${ok ? "installed" : "missing"}`, ok ? "" : "wheelhouse init (installs them again)");
  }

  const daemon = readJson(path.join(DAEMON_HOME, "config.json"), {});
  if (!daemon.deviceId) line("·", "daemon: not paired (only needed to run agents on this machine)", "wheelhouse-daemon pair --server <url>");
  else {
    const running = process.platform !== "darwin" || spawnSync("launchctl", ["print", `gui/${process.getuid()}/com.shipstack.wheelhouse-daemon`], { stdio: "ignore" }).status === 0;
    line(running ? "✓" : "✗", `daemon paired with ${daemon.server} as ${daemon.name}${running ? "" : ", but not running"}`, running ? "" : "wheelhouse-daemon run, or install it as a service");
  }

  for (const [name, it] of Object.entries(INTEGRATIONS)) line(it.installed() ? "✓" : "·", `integration ${name}: ${it.installed() ? "installed" : "not installed"}`, it.installed() ? "" : it.fix);

  if (flags.json) console.log(JSON.stringify(checks, null, 2));
  else for (const c of checks) console.log(`${sym(c.state)} ${c.check}${c.fix ? `\n  ${sym("arrow")} ${c.fix}` : ""}`);
  if (checks.some((c) => c.state === "fail")) process.exit(1);
}

async function cmdWhoami(flags) {
  let root = process.cwd();
  try {
    root = repoRoot();
  } catch {
    /* not in a repo: no wheelhouse.json */
  }
  const cfg = loadConfig(root, flags);
  const check = cfg.url && cfg.key ? await checkKey(cfg.url, cfg.key) : null;
  if (flags.json) return console.log(JSON.stringify({ server: cfg.url || null, team: cfg.team || null, key: cfg.key ? cfg.key.slice(0, 7) + "…" : null, from: cfg.from, check: check?.state ?? null }, null, 2));
  if (!cfg.url) return console.log("No server here: run this in a repo linked with `wheelhouse init`, or pass --url.");
  const from = (k) => (cfg.from[k] ? `  (${cfg.from[k]})` : "");
  console.log(`server:   ${cfg.url}${from("url")}`);
  console.log(`team:     ${cfg.team || "(not set)"}${from("team")}`);
  console.log(`key:      ${cfg.key ? cfg.key.slice(0, 7) + "…" : "(not set)"}${from("key")}`);
  if (!check) return;
  console.log(`check:    ${check.state === "ok" ? `${sym("ok")} key accepted` : check.state === "rejected" ? `${sym("fail")} key rejected: run \`wheelhouse login\`` : `${sym("fail")} unreachable (${check.message})`}`);}

async function cmdStatus(flags) {
  const root = repoRoot();
  const cfg = loadConfig(root, flags);
  console.log(`wheelhouse ${VERSION}`);
  console.log(`repo:     ${root}`);
  console.log(`url:      ${cfg.url || "(not set)"}`);
  console.log(`team:     ${cfg.team || "(not set)"}`);
  console.log(`key:      ${cfg.key ? cfg.key.slice(0, 7) + "…" : "(not set)"}`);
  console.log(`branch:   ${branch(root)} (default ${cfg.defaultBranch})`);
  const dirty = dirtyPaths(root);
  console.log(`dirty:    ${dirty.length} file(s)`);
  const queued = pendingLandings(root).length;
  console.log(`queued:   ${queued} landing(s) waiting to be sent${queued ? " (flushed by the next Claude Code session, or `wheelhouse land --flush`)" : ""}`);
  if (!cfg.configured) return;
  try {
    const b = await call(cfg, "get_briefing", { max_chars: 1500 });
    console.log(`server:   reachable · ${b.stats.openIssues} open issues · ${b.stats.nodes} map nodes · ${b.stats.stale} stale · ${b.stats.inFlux} in flux`);
  } catch (e) {
    console.log(`server:   unreachable (${e.message})`);
  }
}

/** Manual sync for people working without Claude Code: report the working copy as a session. */
async function cmdSync(flags) {
  const root = repoRoot();
  const cfg = loadConfig(root, flags);
  if (!cfg.configured) return console.error("wheelhouse is not configured here (run `wheelhouse init`).");
  const id = flags.session ?? `wc-${os.userInfo().username}-${branch(root)}`;
  const dirty = dirtyPaths(root).filter((f) => !f.startsWith(`${STATE_DIR}/`));
  await call(cfg, "session_sync", { session: id, client: "cli", branch: branch(root), files: hashFiles(root, dirty) });
  const st = await call(cfg, "session_status", { session: id });
  console.log(`✓ reported ${dirty.length} changed file(s) as session ${id}`);
  console.log(st.clean ? "Nothing owed." : st.message);
}

async function cmdLogin(flags) {
  // Scripted form, kept for CI: both flags given, no prompt, no validation round trip needed.
  if (flags.url && typeof flags.key === "string") {
    const url = flags.url.replace(/\/+$/, "");
    saveCredential(url, flags.key);
    rememberDefault({ url });
    console.log(`✓ saved key for ${url} to ${keyStoreName()}`);
    return;
  }
  if (!canPrompt()) {
    console.error("`wheelhouse login` needs a terminal to prompt. In scripts, pass --url and --key, or set WHEELHOUSE_KEY.");
    process.exit(1);
  }
  let root = process.cwd();
  try {
    root = repoRoot();
  } catch {
    /* not in a repo: fall back to the default URL */
  }
  try {
    if (!(await interactiveLogin(root, flags))) process.exit(1);
  } catch (e) {
    if (e.message === "cancelled") process.exit(130);
    throw e;
  }
}

const OVERVIEW = `Board         issues · issue · start · done · comment · new
Setup         login · init · whoami · doctor · status
Daemon        daemon status | pair | install | logs
Integrations  integrations · integrations claude-code install

wheelhouse --help for every command and flag`;

const HELP = `wheelhouse ${VERSION}: Shipstack Wheelhouse CLI

  init --team KEY [--url URL] [--key wh_...] link this repo, install git hooks, record baseline hashes
  login [--url URL]                             prompt (URL defaults to the hosted Wheelhouse) for an API key (hidden), check it, and save it. Scripts: login --url URL --key wh_...
  status                                        show config and server state
  issues [--mine] [--status S] [--cycle current] [--text T] [--json]  list open issues, most urgent first
  issue WH-12 [--json]                          an issue with its description and comments
  start WH-12 [--no-branch]                     claim it, move it to In Progress, switch to branch wh/WH-12-<title>
  done WH-12                                    move it to In Review
  comment WH-12 "text"                          comment on it
  new "title" [--body T] [--priority P] [--cycle current]  create an issue
  daemon status | pair --server URL | unpair | run | install [--dry-run] | uninstall | logs [-n N] [-f]
                                                the agent daemon on this machine (forwards to wheelhouse-daemon)
  doctor [--json]                               check the server, key, repo link, git hooks, daemon and integrations; prints a fix for each problem
  whoami [--json]                               show the server, team and key in use, where each came from, and whether the key works
  context [ISSUE]                               print the team briefing (writes .wheelhouse/context.md) or an issue's context
  sync                                          report working-copy changes as a session (for non-Claude Code workflows)
  land [--baseline|--flush] [--from REF --to REF]  report what landed on the default branch. Git hooks only queue it; --flush sends the queue
  integrations [--json] [NAME COMMAND]          list agent integrations, or run one's command: claude-code install [--from SOURCE] | claude-code hook <session-start | post-edit | pre-bash | post-bash | pre-mcp | prompt | stop | session-end>

--plain: no colour or Unicode (also when output is not a terminal, or NO_COLOR is set).
Config: ${CONFIG_FILE} (url, team, defaultBranch; commit it). Key: WHEELHOUSE_KEY, then 'wheelhouse login' (macOS Keychain, else ~/.wheelhouse/credentials.json), then the plugin's api_key option.
`;

async function main() {
  const [cmd, ...rest] = process.argv.slice(2);
  const flags = parseArgs(rest);
  switch (cmd) {
    case "hook": // hidden alias for `integrations claude-code hook`, while installed plugins update
      return runHook(flags._[0]);
    case "integrations":
      return cmdIntegrations(flags);
    case "init":
      return cmdInit(flags);
    case "login":
      return cmdLogin(flags);
    case "land":
      return cmdLand(flags);
    case "context":
      return cmdContext({ ...flags, _: [cmd, ...flags._] });
    case "status":
      return cmdStatus(flags);
    case "whoami":
      return cmdWhoami(flags);
    case "doctor":
      return cmdDoctor(flags);
    case "daemon":
      return cmdDaemon(flags);
    case "issues":
      return cmdIssues(flags);
    case "issue":
      return cmdIssue(flags);
    case "start":
      return cmdStart(flags);
    case "done":
      return cmdDone(flags);
    case "comment":
      return cmdComment(flags);
    case "new":
      return cmdNew(flags);
    case "sync":
      return cmdSync(flags);
    case "--version":
    case "-v":
      // Scripts get the bare number; a person in a terminal gets the header.
      return console.log(detectLevel() === "plain" ? VERSION : header({ version: VERSION }));
    case "--help":
    case "-h":
    case "help":
      return console.log(HELP);
    default:
      if (cmd) console.error(`unknown command: ${cmd}\n`);
      else if (detectLevel() !== "plain") console.log(header({ version: VERSION }));
      console.log(OVERVIEW);
      if (cmd) process.exitCode = 1;
  }
}

// Piping into `head` etc. closes stdout early; that's not an error.
process.stdout.on("error", (e) => {
  if (e.code === "EPIPE") process.exit(0);
  throw e;
});

main().catch((e) => {
  console.error(e?.message ?? e);
  process.exit(process.argv[2] === "hook" ? 0 : 1); // hooks never fail the agent
});
