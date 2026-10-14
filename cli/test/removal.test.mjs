import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "bin", "wheelhouse.mjs");
let repo, server, port;
const calls = [];
let removal = null; // what the fake server reports as session_removed

before(async () => {
  server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      const tool = req.url.split("/").pop();
      const args = JSON.parse(body || "{}");
      calls.push({ tool, args });
      const data =
        tool === "get_briefing"
          ? { markdown: "# briefing", ...(removal ? { session_removed: removal } : {}) }
          : tool === "get_changes"
            ? { total: 0, changes: [], ...(removal ? { session_removed: removal } : {}) }
            : { ok: true };
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ data }));
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  port = server.address().port;
  repo = fs.mkdtempSync(path.join(os.tmpdir(), "wh-removal-"));
  spawnSync("git", ["init", "-q"], { cwd: repo });
  fs.writeFileSync(path.join(repo, "wheelhouse.json"), JSON.stringify({ url: `http://127.0.0.1:${port}`, project: "QQ" }));
});
after(() => {
  server.close();
  fs.rmSync(repo, { recursive: true, force: true });
});

// The fake server lives in this process, so the hook must run asynchronously.
const hook = (event, input) =>
  new Promise((resolve) => {
    const p = spawn("node", [script, "hook", event], { env: { ...process.env, WHEELHOUSE_KEY: "wh_test" } });
    let out = "";
    p.stdout.on("data", (d) => (out += d));
    p.on("close", () => resolve(out ? JSON.parse(out).hookSpecificOutput?.additionalContext : undefined));
    p.stdin.end(JSON.stringify({ cwd: repo, session_id: "sess-R", ...input }));
  });

test("the briefing tells a reconnected, previously removed session about it and how to answer", async () => {
  removal = { removedAt: "2026-10-12T09:00:00.000Z", missedChanges: 7 };
  const ctx = await hook("session-start", {});
  assert.match(ctx, /teammate removed this session on 2026-10-12/);
  assert.match(ctx, /7 change\(s\)/);
  assert.match(ctx, /resume_session/);
  assert.match(ctx, /replay.*skip/);
  assert.ok(ctx.includes("# briefing"), "the briefing is still there");
});

test("the prompt hook doesn't repeat a removal it already told this session about", async () => {
  // session-start above already notified for this removal
  assert.equal(await hook("prompt", {}), undefined);
});

test("a new removal is reported once, then not again", async () => {
  removal = { removedAt: "2026-10-13T08:00:00.000Z", missedChanges: 2 };
  // the prompt hook only checks the queue every 60s, so clear that throttle for the test
  const f = path.join(repo, ".wheelhouse", "sessions", "sess-R.json");
  const reset = () => fs.writeFileSync(f, JSON.stringify({ ...JSON.parse(fs.readFileSync(f, "utf8")), lastQueueCheck: 0 }));
  reset();
  const first = await hook("prompt", {});
  assert.match(first, /removed this session on 2026-10-13/);
  assert.match(first, /2 change\(s\)/);
  reset();
  assert.equal(await hook("prompt", {}), undefined, "same removal, already told");
});

test("with no removal the hook says nothing about it", async () => {
  removal = null;
  const ctx = await hook("session-start", { session_id: "sess-clean" });
  assert.ok(!/removed this session/.test(ctx));
  assert.ok(calls.some((c) => c.tool === "get_briefing" && c.args.session === "sess-clean"));
});
