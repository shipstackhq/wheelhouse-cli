import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "bin", "wheelhouse.mjs");
let repo;

before(() => {
  repo = fs.mkdtempSync(path.join(os.tmpdir(), "wh-premcp-"));
  spawnSync("git", ["init", "-q"], { cwd: repo });
  fs.writeFileSync(path.join(repo, "wheelhouse.json"), JSON.stringify({ url: "http://127.0.0.1:1", project: "QQ" }));
});
after(() => fs.rmSync(repo, { recursive: true, force: true }));

const run = (input) => {
  const r = spawnSync("node", [script, "hook", "pre-mcp"], { input: JSON.stringify({ cwd: repo, ...input }), env: { ...process.env, WHEELHOUSE_KEY: "wh_test" }, encoding: "utf8" });
  return { code: r.status, out: r.stdout ? JSON.parse(r.stdout) : null };
};

test("adds the session to the tool input and keeps every other field", () => {
  const { out } = run({ session_id: "sess-A", tool_name: "mcp__plugin_wheelhouse_wheelhouse__get_changes", tool_input: { project: "WH", limit: 5 } });
  assert.deepEqual(out.hookSpecificOutput, { hookEventName: "PreToolUse", updatedInput: { project: "WH", limit: 5, session: "sess-A" } });
});

test("overwrites a session the model passed, right or wrong", () => {
  const { out } = run({ session_id: "sess-A", tool_name: "mcp__plugin_wheelhouse_wheelhouse__ack_changes", tool_input: { project: "WH", seq: 9, session: "made-up-by-the-model" } });
  assert.equal(out.hookSpecificOutput.updatedInput.session, "sess-A");
});

test("never sets a permission decision (must not auto-approve Wheelhouse tools)", () => {
  const { out } = run({ session_id: "sess-A", tool_name: "mcp__plugin_wheelhouse_wheelhouse__delete_node", tool_input: { project: "WH", node: "x" } });
  assert.ok(!("permissionDecision" in out.hookSpecificOutput));
});

test("fails open: no session id or no usable input means no output and exit 0", () => {
  for (const input of [{ tool_input: { a: 1 } }, { session_id: "s" }, { session_id: "s", tool_input: "text" }, { session_id: "s", tool_input: [1] }]) {
    const r = run(input);
    assert.equal(r.code, 0);
    assert.equal(r.out, null);
  }
});

test("the plugin's hooks.json wires it to the Wheelhouse MCP tools and the copy is in sync", () => {
  const hooks = JSON.parse(fs.readFileSync(path.join(path.dirname(script), "..", "..", "plugin", "hooks", "hooks.json"), "utf8"));
  const entry = hooks.hooks.PreToolUse.find((e) => e.hooks.some((h) => h.command.endsWith("hook pre-mcp")));
  assert.ok(entry, "pre-mcp hook registered");
  assert.ok(new RegExp(`^(?:${entry.matcher})$`).test("mcp__plugin_wheelhouse_wheelhouse__get_changes"));
  assert.ok(!new RegExp(`^(?:${entry.matcher})$`).test("mcp__other__get_changes"));
});
