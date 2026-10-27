// One job, run by the Claude Code CLI the user installed and signed into. The daemon never touches its credentials: it starts
// `claude -p` in the job's worktree and reads what it prints (stream-json), turning it into short progress events.
//
// Runs are headless, so nobody can answer a permission prompt. --restricted ignores the user's and the repository's settings files
// (no hooks or plugins of theirs run), keeps file tools inside the worktree and refuses bypassPermissions; --permission-prompts none
// refuses anything that would have asked; the allow and deny lists come from the daemon's config.

import { spawn } from "node:child_process";

/** For a continuation: what the earlier run was, how it ended, and what it said, so the agent builds on it instead of starting over. */
function previousAttempt(offer) {
  const p = offer.previous;
  if (!p) return [];
  const how = { succeeded: "finished", failed: "failed", canceled: "was stopped", lost: "was cut off" }[p.state] ?? p.state;
  return [
    `This is a continuation. An earlier attempt at this issue ${how}${p.error ? ` (${p.error})` : ""} and pushed its work to this branch.`,
    p.summary ? `Its final words were:\n> ${p.summary.replace(/\n/g, "\n> ")}` : "It left no summary.",
    "Start by reading what it did (git log and git diff against the base), then carry on from there. Do not redo what is already done.",
    "",
  ];
}

/** Build the agent's instructions from the issue. */
export function buildPrompt(offer) {
  const { issue, repository, branch } = offer;
  return [
    `You are working on the Wheelhouse issue ${issue.ref}: ${issue.title}`,
    "",
    issue.body?.trim() ? issue.body.trim() : "(The issue has no description.)",
    "",
    "---",
    ...previousAttempt(offer),
    offer.previous
      ? `You are in a git worktree of ${repository.fullName ?? repository.name}, on the branch ${branch}, which already holds the earlier attempt's commits. Its pull request (if any) targets ${offer.baseBranch ?? repository.defaultBranch}.`
      : `You are in a fresh git worktree of ${repository.fullName ?? repository.name}, on the branch ${branch}, made from ${offer.baseBranch ?? repository.defaultBranch}.`,
    repository.pathLimit ? `This team's work is limited to ${repository.pathLimit} in this repository.` : null,
    "Make the change the issue asks for. Then check it, in proportion to what you changed: for a Markdown or other docs-only change run no tests at all; for code run the tests, lint and typecheck that cover the files you touched (one package or one test file, not the whole repository's suite), and fix what you broke.",
    "The worktree starts without installed dependencies. Install them only when a check you actually need requires it, never to look around, and prefer the frozen-lockfile form (for example `pnpm install --frozen-lockfile`).",
    `Commit your work on this branch with messages that start with "${issue.ref}: ". Do not push and do not switch branches: Wheelhouse pushes the branch when you finish.`,
    "Finish with a short summary of what you changed and anything left to do.",
  ]
    .filter((l) => l !== null)
    .join("\n");
}

/** Told to every run, so it uses the shell it has instead of giving up after one refusal. */
export const SHELL_RULES = [
  "This run is unattended: nobody can approve a command, so a command that is not pre-approved is refused instead of asked.",
  "Pre-approved: reading commands (ls, cat, head, tail, wc, find, grep, rg, diff, echo, pwd), git status/diff/log/show/rev-parse/ls-files/blame/add/commit/mv/rm, and the project's install, test, lint, typecheck and build commands through npm, pnpm, yarn, npx tsc/eslint/prettier or node --test.",
  "Not allowed: git push, checkout, switch, reset, rebase, worktree, remote or config, deleting branches, rm -rf, curl, wget, ssh and sudo.",
  "A chained command (a && b, a; b, a | b) runs only if every part is pre-approved; when in doubt run one command per call.",
  "One refused command does not mean the others are refused: keep using the approved ones, and commit your work yourself with git add and git commit.",
].join(" ");

export function claudeArgs(offer, runner) {
  return [
    "-p",
    buildPrompt(offer),
    "--model",
    offer.model,
    "--output-format",
    "stream-json",
    "--verbose",
    "--restricted",
    "--strict-mcp-config",
    "--tools",
    runner.tools.join(","),
    "--permission-mode",
    runner.permissionMode,
    "--permission-prompts",
    "none",
    ...(runner.allowedTools.length ? ["--allowedTools", runner.allowedTools.join(",")] : []),
    ...(runner.disallowedTools?.length ? ["--disallowedTools", runner.disallowedTools.join(",")] : []),
    "--settings",
    JSON.stringify({ disableAllHooks: true }),
    "--append-system-prompt",
    SHELL_RULES,
  ];
}

export const short = (s, n = 300) => {
  const t = String(s ?? "").replace(/\s+/g, " ").trim();
  return t.length > n ? `${t.slice(0, n - 1)}…` : t;
};

/** What to show for a tool call: the file, the command, the pattern. */
export function toolSummary(name, input = {}) {
  const mcp = /^mcp__([^_]+)__(.+)$/.exec(name); // an MCP tool: "wheelhouse: get_issue WH-42"
  if (mcp) {
    const arg = [input.ref, input.issue, input.id, input.key, input.query].find((v) => typeof v === "string") ?? (typeof input.title === "string" ? `"${input.title}"` : "");
    return short(`${mcp[1]}: ${mcp[2]} ${arg}`.trim(), 160);
  }
  if (input.file_path) return short(input.file_path, 160);
  if (input.command) return short(input.command, 160);
  if (input.pattern) return short(input.pattern, 120);
  if (input.path) return short(input.path, 160);
  return "";
}

/** A line of stream-json as zero or more progress events. Unknown lines are ignored. */
export function eventsFrom(line) {
  let m;
  try {
    m = JSON.parse(line);
  } catch {
    return [];
  }
  if (m.type === "system" && m.subtype === "init") return [{ type: "started", data: { model: m.model ?? null, sessionId: m.session_id ?? null } }];
  if (m.type === "assistant" && Array.isArray(m.message?.content)) {
    const out = [];
    for (const c of m.message.content) {
      if (c.type === "text" && c.text?.trim()) out.push({ type: "text", data: { text: short(c.text, 600) } });
      if (c.type === "tool_use") out.push({ type: "tool", data: { name: c.name, summary: toolSummary(c.name, c.input) } });
    }
    return out;
  }
  if (m.type === "result")
    return [
      {
        type: "result",
        data: {
          subtype: m.subtype ?? null,
          isError: m.is_error === true,
          numTurns: m.num_turns ?? null,
          costUsd: typeof m.total_cost_usd === "number" ? m.total_cost_usd : null,
          durationMs: m.duration_ms ?? null,
          text: short(m.result, 1000),
        },
      },
    ];
  return [];
}

/** Whether the run stopped on the subscription's usage limit, which every other run would hit too. */
export const isRateLimited = (text) => /usage limit|rate limit|429|limit reached|overloaded/i.test(String(text ?? ""));

/**
 * Run claude in `cwd` with `args`. Resolves { ok, result, stderr, exitCode } when it ends; `signal` stops it (SIGTERM to its process
 * group, then SIGKILL after ten seconds). `parse` turns each stdout line into progress events, `onEvent` gets them; `onSpawn` the
 * child's pid, for the job record. Shared by jobs and chats.
 */
export function runCli({ args, cwd, claudePath = "claude", signal, parse = eventsFrom, onEvent = () => {}, onSpawn = () => {}, timeoutMs }) {
  return new Promise((resolve) => {
    const child = spawn(claudePath, args, { cwd, detached: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, CLAUDE_CODE_ENTRYPOINT: "wheelhouse-daemon" } });
    onSpawn(child.pid);
    let result = null;
    let stderr = "";
    let buf = "";
    let killed = null;
    const stop = (why) => {
      if (killed || child.exitCode !== null) return;
      killed = why;
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        /* already gone */
      }
      setTimeout(() => {
        try {
          process.kill(-child.pid, "SIGKILL");
        } catch {
          /* already gone */
        }
      }, 10_000).unref();
    };
    const timer = setTimeout(() => stop("timeout"), timeoutMs);
    const onAbort = () => stop("canceled");
    signal?.addEventListener("abort", onAbort);
    if (signal?.aborted) onAbort();

    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (d) => {
      buf += d;
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        for (const e of parse(line)) {
          if (e.type === "result") result = e.data;
          onEvent(e);
        }
      }
    });
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (d) => (stderr = (stderr + d).slice(-4000)));
    const done = (exitCode, error) => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      resolve({ ok: !killed && exitCode === 0 && result && !result.isError, exitCode, result, stderr: short(stderr, 1000), stopped: killed, error });
    };
    child.on("error", (e) => done(null, e.code === "ENOENT" ? `claude not found at "${claudePath}"` : e.message));
    child.on("close", (code) => done(code, null));
  });
}

/** Run one job's agent in `cwd`. `timeoutMs` defaults to the runner's limit. */
export const runClaude = ({ offer, cwd, runner, timeoutMs = runner.timeoutMinutes * 60_000, ...rest }) => runCli({ args: claudeArgs(offer, runner), cwd, timeoutMs, ...rest });
