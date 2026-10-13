#!/usr/bin/env node
// wheelhouse: CLI + Claude Code hook runner for Shipstack Wheelhouse.
//
// Zero dependencies (Node >= 18). The Claude Code plugin bundles this file, so
// it must stay self-contained. Content hashes are git blob ids (`git hash-object`),
// which match committed blobs exactly.
//
// Every hook fails OPEN: if Wheelhouse is unreachable or anything throws, the
// agent's work continues and the error is logged to .wheelhouse/hook.log.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const VERSION = "0.1.2";
const CONFIG_FILE = "wheelhouse.json"; // committed: { url, project, defaultBranch }
const STATE_DIR = ".wheelhouse"; // gitignored: context.md, sessions/, hook.log
const CRED_FILE = path.join(os.homedir(), ".config", "wheelhouse", "credentials.json");
const EDIT_TOOLS = new Set(["Edit", "Write", "MultiEdit", "NotebookEdit"]);
const QUEUE_DIGEST = 10; // changes injected per prompt; the rest stay queued for get_changes

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
  const url = (flags.url || process.env.WHEELHOUSE_URL || process.env.CLAUDE_PLUGIN_OPTION_SERVER_URL || file.url || "").replace(/\/+$/, "");
  const creds = readJson(CRED_FILE, {});
  const key = flags.key || process.env.WHEELHOUSE_KEY || process.env.CLAUDE_PLUGIN_OPTION_API_KEY || (url && creds[url]) || "";
  const project = flags.project || process.env.WHEELHOUSE_PROJECT || file.project || "";
  const defaultBranch = file.defaultBranch || detectDefaultBranch(root) || "main";
  return { url, key, project, defaultBranch, configured: !!(url && key && project) };
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
      body: JSON.stringify({ project: cfg.project, ...args }),
      signal: ctl.signal,
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(`${tool}: ${res.status} ${body?.error?.message ?? ""}`.trim());
    return body.data;
  } finally {
    clearTimeout(t);
  }
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
    await autoLand(cfg, root, id).catch((e) => log(root, `session-start land: ${e.message}`));
    await call(cfg, "session_sync", { session: id, client: "claude-code", branch: branch(root), files: [] }).catch((e) => log(root, `session-start sync: ${e.message}`));
    // With `session`, the briefing registers this session's own queue cursor and tells the agent its id.
    const b = await call(cfg, "get_briefing", { max_chars: 6000, session: id }, 6000);
    const head = git(["rev-parse", "--short", "HEAD"], { cwd: root }) ?? "unknown";
    const md = `<!-- generated by wheelhouse ${VERSION} at ${new Date().toISOString()} for ${head}; do not edit -->\n${b.markdown}\n`;
    fs.mkdirSync(path.join(root, STATE_DIR), { recursive: true });
    fs.writeFileSync(path.join(root, STATE_DIR, "context.md"), md);
    return {
      hookSpecificOutput: {
        hookEventName: "SessionStart",
        additionalContext: `${b.markdown}\n\n(Wheelhouse project ${cfg.project}. This briefing is also saved at ${STATE_DIR}/context.md. Before working a ticket call get_ticket_context. The board and map are kept in sync by hooks: changed files are tracked automatically, and before you finish a turn you'll be asked to update the ticket and map if needed.)`,
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
      if (q?.total) {
        const shown = q.changes;
        const lines = shown.map((c) => `- ${c.actorName} ${c.summary}`).join("\n");
        const more = q.total - shown.length;
        context = `Wheelhouse: ${q.total} new change(s) from the team in ${cfg.project} since you last checked:\n${lines}${
          more > 0 ? `\n…and ${more} more. Call get_changes to see them, then ack_changes.` : ""
        }\nHandle anything addressed to you (these are already marked seen for this session).`;
        const last = shown.at(-1)?.seq;
        if (last) await call(cfg, "ack_changes", { seq: last, session: id }, 3000).catch((e) => log(root, `prompt ack: ${e.message}`));
      }
    }
    saveSession(root, id, s);
    return context ? { hookSpecificOutput: { hookEventName: "UserPromptSubmit", additionalContext: context } } : undefined;
  },

  /** Stop: refresh hashes, then block the turn (at most twice per prompt) while the session owes updates. */
  async stop(cfg, root, input) {
    const id = input.session_id;
    const s = loadSession(root, id);
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
      return { systemMessage: `Wheelhouse: this session still has unsynced work in ${cfg.project} (see the ticket/map). It will be flagged on the board.` };
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
  const url = (flags.url || existing.url || process.env.WHEELHOUSE_URL || "").replace(/\/+$/, "");
  const project = (flags.project || existing.project || "").toUpperCase();
  if (!url || !project) {
    console.error("Usage: wheelhouse init --url <https://wheelhouse.example> --project <KEY> [--key wh_...] [--no-git-hooks] [--no-baseline]");
    process.exit(1);
  }
  const defaultBranch = flags.branch || existing.defaultBranch || detectDefaultBranch(root) || "main";
  writeJson(path.join(root, CONFIG_FILE), { url, project, defaultBranch });
  console.log(`✓ wrote ${CONFIG_FILE} (url ${url}, project ${project}, default branch ${defaultBranch})`);

  if (flags.key) {
    const creds = readJson(CRED_FILE, {});
    creds[url] = flags.key;
    writeJson(CRED_FILE, creds, 0o600);
    console.log(`✓ saved API key for ${url} to ${CRED_FILE}`);
  }

  // .gitignore
  const gi = path.join(root, ".gitignore");
  const cur = fs.existsSync(gi) ? fs.readFileSync(gi, "utf8") : "";
  if (!cur.split("\n").some((l) => l.trim() === `${STATE_DIR}/` || l.trim() === STATE_DIR)) {
    fs.writeFileSync(gi, cur + (cur.endsWith("\n") || !cur ? "" : "\n") + `${STATE_DIR}/\n`);
    console.log(`✓ added ${STATE_DIR}/ to .gitignore`);
  }

  if (!flags["no-git-hooks"]) installGitHooks(root);

  const cfg = loadConfig(root, flags);
  if (!cfg.key) {
    console.log(`! no API key yet: run \`wheelhouse login --url ${url} --key wh_...\`, set WHEELHOUSE_KEY, or configure it in the Claude Code plugin.`);
    return;
  }
  try {
    const projects = await call(cfg, "list_projects", {});
    if (!projects.some((p) => p.key === project)) console.log(`! project ${project} not found on ${url}; create it in the web app.`);
    else console.log(`✓ connected to ${url} as an agent key; project ${project} found`);
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
async function cmdBaseline(cfg, root) {
  const tree = git(["ls-tree", "-r", "--full-tree", cfg.defaultBranch], { cwd: root }) ?? git(["ls-tree", "-r", "--full-tree", "HEAD"], { cwd: root });
  if (!tree) return console.log("! no commits yet; skipped baseline");
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
  console.log(`✓ recorded baseline hashes for ${entries.length} files on ${cfg.defaultBranch}`);
}

/** Report what landed on the default branch (git hooks / CI). */
async function cmdLand(flags) {
  const root = repoRoot();
  const cfg = loadConfig(root, flags);
  if (!cfg.configured) {
    if (!flags["git-hook"]) console.error("wheelhouse is not configured here (run `wheelhouse init`).");
    return;
  }
  if (flags.baseline) return cmdBaseline(cfg, root);
  const current = branch(root);
  if (current !== cfg.defaultBranch && !flags.force) {
    if (!flags["git-hook"]) console.log(`On ${current}, not ${cfg.defaultBranch}: nothing has landed. (Use --force to report anyway.)`);
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
  try {
    const r = await call(cfg, "land_changes", { sha, branch: current, files }, 15000);
    if (to === "HEAD" && sha) writeJson(path.join(root, STATE_DIR, "landed.json"), { sha });
    if (!flags["git-hook"])
      console.log(`✓ landed ${r.files} file(s)${r.published.length ? `; published drafts: ${r.published.join(", ")}` : ""}${r.stale.length ? `; drifted: ${r.stale.join(", ")}` : ""}${r.fresh.length ? `; back in sync: ${r.fresh.join(", ")}` : ""}`);
  } catch (e) {
    log(root, `land: ${e.message}`);
    if (!flags["git-hook"]) console.error(`! ${e.message}`);
  }
}

async function cmdContext(flags) {
  const root = repoRoot();
  const cfg = loadConfig(root, flags);
  if (!cfg.configured) return console.error("wheelhouse is not configured here (run `wheelhouse init`).");
  const ticket = flags.ticket ?? flags._[1];
  const r = ticket ? await call(cfg, "get_ticket_context", { ticket }, 8000) : await call(cfg, "get_briefing", {}, 8000);
  if (!ticket) {
    fs.mkdirSync(path.join(root, STATE_DIR), { recursive: true });
    fs.writeFileSync(path.join(root, STATE_DIR, "context.md"), r.markdown + "\n");
  }
  process.stdout.write(r.markdown + "\n");
}

async function cmdStatus(flags) {
  const root = repoRoot();
  const cfg = loadConfig(root, flags);
  console.log(`wheelhouse ${VERSION}`);
  console.log(`repo:     ${root}`);
  console.log(`url:      ${cfg.url || "(not set)"}`);
  console.log(`project:  ${cfg.project || "(not set)"}`);
  console.log(`key:      ${cfg.key ? cfg.key.slice(0, 7) + "…" : "(not set)"}`);
  console.log(`branch:   ${branch(root)} (default ${cfg.defaultBranch})`);
  const dirty = dirtyPaths(root);
  console.log(`dirty:    ${dirty.length} file(s)`);
  if (!cfg.configured) return;
  try {
    const b = await call(cfg, "get_briefing", { max_chars: 1500 });
    console.log(`server:   reachable · ${b.stats.openTickets} open tickets · ${b.stats.nodes} map nodes · ${b.stats.stale} stale · ${b.stats.inFlux} in flux`);
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

function cmdLogin(flags) {
  if (!flags.url || !flags.key) {
    console.error("Usage: wheelhouse login --url <https://wheelhouse.example> --key wh_...");
    process.exit(1);
  }
  const creds = readJson(CRED_FILE, {});
  creds[flags.url.replace(/\/+$/, "")] = flags.key;
  writeJson(CRED_FILE, creds, 0o600);
  console.log(`✓ saved key for ${flags.url} to ${CRED_FILE}`);
}

const HELP = `wheelhouse ${VERSION}: Shipstack Wheelhouse CLI

  init --url URL --project KEY [--key wh_...]   link this repo, install git hooks, record baseline hashes
  login --url URL --key wh_...                  store an API key for a server
  status                                        show config and server state
  context [TICKET]                              print the project briefing (writes .wheelhouse/context.md) or a ticket's context
  sync                                          report working-copy changes as a session (for non-Claude Code workflows)
  land [--baseline] [--from REF --to REF]       report what landed on the default branch (git hooks run this)
  hook <event>                                  Claude Code hook runner: session-start | post-edit | pre-bash | post-bash | prompt | stop | session-end

Config: ${CONFIG_FILE} (url, project, defaultBranch; commit it). Key: WHEELHOUSE_KEY, the plugin's api_key option, or ~/.config/wheelhouse/credentials.json.
`;

async function main() {
  const [cmd, ...rest] = process.argv.slice(2);
  const flags = parseArgs(rest);
  switch (cmd) {
    case "hook":
      return runHook(flags._[0]);
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
    case "sync":
      return cmdSync(flags);
    case "--version":
    case "-v":
      return console.log(VERSION);
    default:
      return console.log(HELP);
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
