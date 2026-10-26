import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { DEFAULT_RUNNER } from "../src/config.mjs";
import { chatArgs, chatParser, runChat } from "../src/runner/chat.mjs";
import { buildPrompt, claudeArgs, eventsFrom, runClaude } from "../src/runner/claude.mjs";

// A stand-in for the claude CLI: prints stream-json like `claude -p --output-format stream-json`, records its arguments, and with
// FAKE_CLAUDE_HANG stays alive until it is killed.
let tmp, fake;
before(() => {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "wh-daemon-runner-"));
  fake = path.join(tmp, "claude");
  fs.writeFileSync(
    fake,
    `#!/usr/bin/env node
require("fs").writeFileSync(require("path").join(process.cwd(), "args.json"), JSON.stringify(process.argv.slice(2)));
const out = (o) => process.stdout.write(JSON.stringify(o) + "\\n");
out({ type: "system", subtype: "init", model: "claude-sonnet", session_id: "s1" });
out({ type: "assistant", message: { content: [{ type: "text", text: "Looking at the code" }, { type: "tool_use", name: "Edit", input: { file_path: "src/a.ts" } }] } });
if (process.env.FAKE_CLAUDE_HANG) setInterval(() => {}, 1000);
else out({ type: "result", subtype: "success", is_error: false, num_turns: 3, total_cost_usd: 0.12, duration_ms: 900, result: "Changed a.ts" });
`,
    { mode: 0o755 },
  );
});
after(() => fs.rmSync(tmp, { recursive: true, force: true }));

const offer = { jobId: "j", model: "sonnet", branch: "wh/WH-9-fix", issue: { ref: "WH-9", title: "Fix it", body: "Steps to reproduce" }, repository: { name: "app", fullName: "acme/app", defaultBranch: "main", pathLimit: null } };

test("the prompt carries the issue and the rules of the run", () => {
  const p = buildPrompt(offer);
  assert.match(p, /WH-9: Fix it/);
  assert.match(p, /Steps to reproduce/);
  assert.match(p, /Do not push/);
  assert.match(p, /docs-only change run no tests/);
  assert.match(p, /not the whole repository's suite/);
  assert.match(p, /frozen-lockfile/);
});

test("a continuation tells the agent about the earlier attempt and that the branch already has its commits", () => {
  const p = buildPrompt({ ...offer, startFrom: offer.branch, previous: { jobId: "j0", state: "failed", summary: "Edited a.ts\nTests still fail", error: "timed out" } });
  assert.match(p, /This is a continuation/);
  assert.match(p, /failed \(timed out\)/);
  assert.match(p, /> Edited a\.ts\n> Tests still fail/);
  assert.match(p, /already holds the earlier attempt's commits/);
  assert.doesNotMatch(buildPrompt(offer), /continuation/);
});

test("the run is headless and fenced: restricted, no MCP, no hooks, prompts refused, push denied", () => {
  const a = claudeArgs(offer, DEFAULT_RUNNER);
  for (const f of ["-p", "--restricted", "--strict-mcp-config", "--verbose"]) assert.ok(a.includes(f), f);
  assert.equal(a[a.indexOf("--model") + 1], "sonnet");
  assert.equal(a[a.indexOf("--output-format") + 1], "stream-json");
  assert.equal(a[a.indexOf("--permission-prompts") + 1], "none");
  assert.match(a[a.indexOf("--disallowedTools") + 1], /git push/);
  assert.deepEqual(JSON.parse(a[a.indexOf("--settings") + 1]), { disableAllHooks: true });
  assert.equal(a.includes("--dangerously-skip-permissions"), false);
  assert.match(a[a.indexOf("--append-system-prompt") + 1], /does not mean the others are refused/);
  const allowed = a[a.indexOf("--allowedTools") + 1].split(",");
  for (const r of ["Bash(git commit:*)", "Bash(cat:*)", "Bash(pnpm test:*)"]) assert.ok(allowed.includes(r), r);
  for (const r of ["Bash(curl:*)", "Bash(git push:*)"]) assert.ok(!allowed.includes(r), r);
});

test("stream-json becomes short progress events", () => {
  assert.deepEqual(eventsFrom('{"type":"system","subtype":"init","model":"m","session_id":"s"}'), [{ type: "started", data: { model: "m", sessionId: "s" } }]);
  assert.deepEqual(eventsFrom("not json"), []);
  assert.deepEqual(eventsFrom('{"type":"user"}'), []);
});

test("runs the CLI in the worktree and reports its result", async () => {
  const cwd = fs.mkdtempSync(path.join(tmp, "wt-"));
  const events = [];
  const r = await runClaude({ offer, cwd, runner: DEFAULT_RUNNER, claudePath: fake, onEvent: (e) => events.push(e) });
  assert.equal(r.ok, true);
  assert.equal(r.result.costUsd, 0.12);
  assert.deepEqual(events.map((e) => e.type), ["started", "text", "tool", "result"]);
  assert.deepEqual(events[2].data, { name: "Edit", summary: "src/a.ts" });
  assert.equal(JSON.parse(fs.readFileSync(path.join(cwd, "args.json"), "utf8"))[0], "-p");
});

test("an abort stops the agent and its children", async () => {
  const cwd = fs.mkdtempSync(path.join(tmp, "wt-"));
  const stop = new AbortController();
  process.env.FAKE_CLAUDE_HANG = "1";
  try {
    const p = runClaude({ offer, cwd, runner: DEFAULT_RUNNER, claudePath: fake, signal: stop.signal, onEvent: (e) => e.type === "tool" && stop.abort() });
    const r = await p;
    assert.equal(r.ok, false);
    assert.equal(r.stopped, "canceled");
  } finally {
    delete process.env.FAKE_CLAUDE_HANG;
  }
});

test("a missing CLI is a clear failure", async () => {
  const r = await runClaude({ offer, cwd: tmp, runner: DEFAULT_RUNNER, claudePath: path.join(tmp, "nope") });
  assert.equal(r.ok, false);
  assert.match(r.error, /not found/);
});

const chatMsg = { messageId: "m", threadId: "t", model: "opus", sessionId: null, prompt: "Where is auth?", repository: { name: "app", fullName: "acme/app", defaultBranch: "main" } };

const at = { commit: "abc1234", branches: ["origin/main"] };

test("a chat run is read-only: file tools and read-only git, no MCP, no hooks, partial messages, the prompt as typed", () => {
  const a = chatArgs(chatMsg, at);
  assert.equal(a[0], "-p");
  assert.equal(a[1], "Where is auth?");
  for (const f of ["--restricted", "--strict-mcp-config", "--verbose", "--include-partial-messages"]) assert.ok(a.includes(f), f);
  assert.equal(a[a.indexOf("--model") + 1], "opus");
  assert.equal(a[a.indexOf("--tools") + 1], "Read,Grep,Glob,Bash");
  assert.equal(a[a.indexOf("--permission-prompts") + 1], "none");
  const allowed = a[a.indexOf("--allowedTools") + 1].split(",");
  for (const t of ["Read", "Grep", "Glob", "Bash(git log:*)", "Bash(git diff:*)", "Bash(git show:*)", "Bash(git branch -r:*)", "Bash(git checkout --detach:*)"]) assert.ok(allowed.includes(t), t);
  // Nothing that writes to the shared clone: no bare `git branch` (it deletes with -D), no plain checkout, no push or fetch.
  for (const t of ["Bash(git branch:*)", "Bash(git checkout:*)", "Bash(git push:*)", "Bash(git fetch:*)", "Bash"]) assert.ok(!allowed.includes(t), t);
  assert.equal(a.includes("--resume"), false);
  assert.deepEqual(JSON.parse(a[a.indexOf("--settings") + 1]), { disableAllHooks: true });
  const rules = a[a.indexOf("--append-system-prompt") + 1];
  assert.match(rules, /acme\/app.*abc1234 \(the tip of origin\/main\).*cite the file paths/);
  assert.match(rules, /git checkout --detach origin\/<branch>/);
  assert.match(chatArgs(chatMsg, { commit: "def5678", branches: [] })[a.indexOf("--append-system-prompt") + 1], /now at commit def5678;/);
  const resumed = chatArgs({ ...chatMsg, sessionId: "s9" }, at);
  assert.equal(resumed[resumed.indexOf("--resume") + 1], "s9");
});

const line = (o) => JSON.stringify(o);
const delta = (text) => line({ type: "stream_event", event: { type: "content_block_delta", delta: { type: "text_delta", text } } });
const assistant = line({ type: "assistant", message: { content: [{ type: "text", text: "Full text" }, { type: "tool_use", name: "Grep", input: { pattern: "auth" } }] } });
const result = line({ type: "result", is_error: false, session_id: "s1", total_cost_usd: 0.03, result: "Full text" });

test("the chat parser: text deltas, tool calls, and the result; the full text only when no deltas came", () => {
  const p = chatParser();
  assert.deepEqual(p(delta("Full ")), [{ type: "delta", data: { text: "Full " } }]);
  assert.deepEqual(p(line({ type: "stream_event", event: { type: "message_start" } })), []);
  assert.deepEqual(p("not json"), []);
  assert.deepEqual(p(assistant), [{ type: "tool", data: { name: "Grep", summary: "auth" } }], "text already streamed");
  assert.deepEqual(p(result), [{ type: "result", data: { isError: false, sessionId: "s1", costUsd: 0.03, text: "Full text" } }]);
  assert.deepEqual(chatParser()(assistant), [{ type: "delta", data: { text: "Full text" } }, { type: "tool", data: { name: "Grep", summary: "auth" } }], "no partials: fall back");
});

test("runs a chat message and reports deltas, tools and the whole answer; an abort stops it", async () => {
  const cwd = fs.mkdtempSync(path.join(tmp, "chat-"));
  const chatFake = path.join(tmp, "claude-chat");
  fs.writeFileSync(
    chatFake,
    `#!/usr/bin/env node
const out = (o) => process.stdout.write(JSON.stringify(o) + "\\n");
out({ type: "stream_event", event: { type: "content_block_delta", delta: { type: "text_delta", text: "Hel" } } });
out({ type: "assistant", message: { content: [{ type: "tool_use", name: "Read", input: { file_path: "a.ts" } }] } });
if (process.env.FAKE_CLAUDE_HANG) setInterval(() => {}, 1000);
else out({ type: "result", is_error: false, session_id: "s1", total_cost_usd: 0.01, result: "Hello" });
`,
    { mode: 0o755 },
  );
  const events = [];
  const r = await runChat({ message: chatMsg, checkout: at, cwd, claudePath: chatFake, onEvent: (e) => events.push(e) });
  assert.equal(r.ok, true);
  assert.equal(r.result.text, "Hello");
  assert.deepEqual(events.map((e) => e.type), ["delta", "tool", "result"]);

  const stop = new AbortController();
  process.env.FAKE_CLAUDE_HANG = "1";
  try {
    const hung = await runChat({ message: chatMsg, checkout: at, cwd, claudePath: chatFake, signal: stop.signal, onEvent: (e) => e.type === "tool" && stop.abort() });
    assert.equal(hung.stopped, "canceled");
  } finally {
    delete process.env.FAKE_CLAUDE_HANG;
  }
});
