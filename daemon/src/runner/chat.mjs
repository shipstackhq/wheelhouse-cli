// One chat message, answered by the same `claude` CLI as jobs but fenced tighter: read-only tools (Read, Grep, Glob) and read-only git
// (plus moving its own detached checkout to another branch), no other shell, no MCP, no hooks, over a detached worktree of the repository. The prompt is the person's message as typed; what the assistant is and
// may do goes in the system prompt. stream-json comes back as `delta` (answer text), `tool` (what it looked at) and `result`.

import { runCli, toolSummary } from "./claude.mjs";

/** A chat answer that takes longer than this is stopped. */
export const CHAT_TIMEOUT_MS = 10 * 60_000;

/**
 * The git commands a chat may run: reading history and other branches, and moving its own checkout (a throwaway detached worktree) to
 * another branch. `git branch` only with -r/-a, since a bare `git branch -D` would delete a run's branch in the shared clone.
 */
export const CHAT_GIT = ["log", "diff", "show", "blame", "grep", "status", "ls-files", "ls-tree", "rev-parse", "merge-base", "branch -r", "branch -a", "checkout --detach"].map((c) => `Bash(git ${c}:*)`);

/** Where the checkout is: its commit, and the branches (origin/…) at that commit, if any. */
const where = ({ commit, branches = [] }) => `commit ${commit}${branches.length ? ` (the tip of ${branches.join(", ")})` : ""}`;

/** What the assistant is, and what it cannot do, told to every message. */
export const chatRules = (repository, checkout) =>
  [
    `You are a read-only assistant answering questions about the repository ${repository.fullName ?? repository.name}. Your working directory is a checkout of it, now at ${where(checkout)}; its default branch is ${repository.defaultBranch}. Wheelhouse fetches every remote branch just before each message, so the origin/<name> refs are already up to date: never run git fetch or pull (they are not allowed), and do not warn that branches may be stale.`,
    "Answer from the code: look at the files before you answer, and cite the file paths (and the branch, when it is not the checked-out one) you relied on.",
    "Say so when you are unsure or cannot find something; do not guess.",
    "Besides Read, Grep and Glob you may run read-only git: git log, diff, show, blame, grep, status, ls-files, ls-tree, rev-parse, merge-base, and git branch -r or -a to list branches. Use them for history and for other branches (for example `git diff origin/main...origin/development --stat` or `git show origin/staging:path/to/file`).",
    "When asked to look at or switch to another branch, run `git checkout --detach origin/<branch>`; the checkout stays there for the rest of this conversation, so say which branch you are on when it matters.",
    "Run one command per call: chained or piped commands are refused. You cannot run any other command or change files, so do not offer to, and say so if asked to.",
    "You have no access to Wheelhouse issues or the board yet.",
  ].join(" ");

export function chatArgs(message, checkout) {
  return [
    "-p",
    message.prompt,
    "--model",
    message.model,
    ...(message.sessionId ? ["--resume", message.sessionId] : []),
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--restricted",
    "--strict-mcp-config",
    "--tools",
    "Read,Grep,Glob,Bash",
    "--permission-mode",
    "manual",
    "--permission-prompts",
    "none",
    "--allowedTools",
    ["Read", "Grep", "Glob", ...CHAT_GIT].join(","),
    "--settings",
    JSON.stringify({ disableAllHooks: true }),
    "--append-system-prompt",
    chatRules(message.repository, checkout),
  ];
}

/**
 * A parser for one run's stdout lines (it remembers whether partial text arrived). `stream_event` text deltas become `delta`; the
 * finished `assistant` message gives each `tool_use` as `tool`, and its text as one `delta` only when no partials came (older CLIs).
 */
export function chatParser() {
  let partials = false;
  return (line) => {
    let m;
    try {
      m = JSON.parse(line);
    } catch {
      return [];
    }
    if (m.type === "stream_event") {
      const d = m.event?.type === "content_block_delta" ? m.event.delta : null;
      if (d?.type !== "text_delta" || !d.text) return [];
      partials = true;
      return [{ type: "delta", data: { text: d.text } }];
    }
    if (m.type === "assistant" && Array.isArray(m.message?.content)) {
      const out = [];
      for (const c of m.message.content) {
        if (c.type === "text" && c.text && !partials) out.push({ type: "delta", data: { text: c.text } });
        if (c.type === "tool_use") out.push({ type: "tool", data: { name: c.name, summary: toolSummary(c.name, c.input) } });
      }
      return out;
    }
    if (m.type === "result")
      return [
        {
          type: "result",
          data: {
            isError: m.is_error === true,
            sessionId: m.session_id ?? null,
            costUsd: typeof m.total_cost_usd === "number" ? m.total_cost_usd : null,
            text: String(m.result ?? ""),
          },
        },
      ];
    return [];
  };
}

/** Answer one chat message in `cwd` (the thread's worktree, where `checkout` says it is). Resolves like runClaude; `result.text` is the whole answer. */
export const runChat = ({ message, checkout, cwd, claudePath, signal, onEvent, onSpawn, timeoutMs = CHAT_TIMEOUT_MS }) =>
  runCli({ args: chatArgs(message, checkout), cwd, claudePath, signal, parse: chatParser(), onEvent, onSpawn, timeoutMs });
