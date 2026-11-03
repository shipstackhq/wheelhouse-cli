// Runs the Node daemon's runner logic on fixed inputs and writes the expected outputs: rust/fixtures/runner.json.
// The Rust tests in crates/wh-core compare their output with these, byte for byte.
// Usage: node rust/tools/gen-runner-golden.mjs   (CI runs it before the Rust tests; the output is not committed)

// The output must not depend on the machine: paths are scrubbed against a fixed home, which os.homedir() reads from HOME.
process.env.HOME = "/Users/example";

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_RUNNER } from "../../daemon/src/config.mjs";
import { buildPrompt, claudeArgs, eventsFrom, isRateLimited, scrub, SHELL_RULES, toolSummary } from "../../daemon/src/runner/claude.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.join(here, "..", "fixtures", "runner.json");

const repository = { id: "11111111-1111-1111-1111-111111111111", name: "app", fullName: "acme/app", cloneUrl: "https://example.invalid/acme/app.git", defaultBranch: "main" };
const issue = { ref: "WH-42", title: "Add a footer", body: "  Put a footer on every page.\n\nKeep it short.  ", team: { key: "WH" } };
const base = { issue, repository, branch: "wh/WH-42-footer", model: "sonnet", mode: "job" };

const offers = {
  plain: { ...base },
  noBody: { ...base, issue: { ...issue, body: "  " } },
  continuationAnswered: {
    ...base,
    previous: { state: "succeeded", summary: "QUESTION: Which colour?\nShould it be blue?", answer: "Blue, please.\nNo gradients." },
  },
  continuationFailed: { ...base, previous: { state: "failed", error: "tests broke", summary: "Stopped at step 2.\nSee the log." } },
  continuationLost: { ...base, previous: { state: "lost", summary: null } },
  continuationOther: { ...base, previous: { state: "weird", summary: "x" } },
  fixes: {
    ...base,
    fix: [
      { name: "unit", command: "npm test", exitCode: 1, tail: "not ok 3\n  expected 2" },
      { name: "lint", command: "npm run lint", exitCode: 2, tail: "error: unused var" },
    ],
  },
  baseMerged: { ...base, baseSync: "merged", baseBranch: "staging" },
  baseConflict: { ...base, baseSync: ["src/a.ts", "README.md"] },
  baseConflictNone: { ...base, baseSync: [] },
  pathLimit: { ...base, repository: { ...repository, pathLimit: "packages/web" } },
  agent: { ...base, agent: { name: "footer-writer", definitions: { "footer-writer": { description: "x" } } }, effort: "high" },
  mcpReadOnly: { ...base, mcp: { token: "t0k" } },
  review: {
    ...base,
    mode: "review",
    review: { pullRequest: { number: 7, url: "https://github.example/acme/app/pull/7", headBranch: "wh/WH-42-footer" } },
    mcp: { token: "t0k" },
    issue: { ...issue, body: null },
  },
  reviewNoPr: { ...base, mode: "review" },
};

const runner = DEFAULT_RUNNER;
const cases = {
  offers,
  defaultRunner: runner,
  shellRules: SHELL_RULES,
  prompts: Object.fromEntries(Object.entries(offers).map(([k, o]) => [k, buildPrompt(o)])),
  args: {
    plain: claudeArgs(offers.plain, runner, undefined),
    effortAndAgent: claudeArgs(offers.agent, runner, undefined),
    mcp: claudeArgs(offers.mcpReadOnly, runner, "/tmp/wh-mcp/mcp.json"),
    review: claudeArgs(offers.review, runner, "/tmp/wh-mcp/mcp.json"),
    reviewNoMcp: claudeArgs(offers.reviewNoPr, runner, undefined),
    restrictedRunner: claudeArgs(offers.plain, { ...runner, disallowedTools: [] , allowedTools: ["Bash(ls:*)"] }, undefined),
  },
  scrub: [
    ["plain text", "nothing to see"],
    ["home path", "/Users/example/projects/app/src/x.ts"],
    ["chat folder", "/Users/example/.wheelhouse/daemon/chats/11111111/22222222/src/a.ts"],
    ["worktree root", "/Users/example/.wheelhouse/daemon/worktrees/11111111/22222222/"],
    ["worktree bare", "cd /Users/example/.wheelhouse/daemon/worktrees/11111111/22222222"],
    ["mixed", "/Users/example/.wheelhouse/daemon/worktrees/a/b/c and /Users/example/x"],
    ["regex chars", "a.b (c) [d] /Users/example.x/y"],
  ].map(([name, input]) => ({ name, input, output: scrub(input, "/Users/example") })),
  toolSummary: [
    ["Read", { file_path: "/Users/example/a.ts" }],
    ["Bash", { command: "npm test -- --grep " + "x".repeat(200) }],
    ["Grep", { pattern: "needle" }],
    ["Glob", { path: "src" }],
    ["mcp", { ref: "WH-9" }],
    ["mcp title", { title: "A title" }],
    ["none", {}],
  ].map(([name, input]) => ({ name, input, output: toolSummary(name, input) })),
  eventsFrom: [
    '{"type":"system","subtype":"init","model":"sonnet","session_id":"s1"}',
    '{"type":"assistant","message":{"content":[{"type":"text","text":"Hello   world\\n"},{"type":"tool_use","name":"Read","input":{"file_path":"/Users/example/a.ts"}}]}}',
    '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.12,"duration_ms":900,"result":"Done.\\nLEARNING: use pnpm\\nLEARNING: keep tests small"}',
    '{"type":"result","subtype":"error_max_turns","is_error":true,"result":"stopped"}',
    "not json at all",
    '{"type":"user"}',
  ].map((line) => ({ line, events: eventsFrom(line) })),
  isRateLimited: ["usage limit reached", "HTTP 429", "all good", "Overloaded"].map((s) => ({ input: s, output: isRateLimited(s) })),
};

fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(out, JSON.stringify(cases, null, 2) + "\n");
console.log(`wrote ${out}`);
