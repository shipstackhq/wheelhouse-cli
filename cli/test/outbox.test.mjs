import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "bin", "wheelhouse.mjs");
let repo, home, server, port;
const calls = [];
let landStatus = 200; // what the fake server answers to land_changes

before(async () => {
  server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      const tool = req.url.split("/").pop();
      const args = JSON.parse(body || "{}");
      calls.push({ tool, args, auth: req.headers.authorization });
      res.setHeader("content-type", "application/json");
      if (tool === "land_changes" && landStatus !== 200) {
        res.statusCode = landStatus;
        return res.end(JSON.stringify({ error: { message: "nope" } }));
      }
      const data =
        tool === "get_briefing" ? { markdown: "# briefing" }
        : tool === "land_changes" ? { files: 1, published: [], stale: [], fresh: [] }
        : tool === "get_changes" ? { total: 0, changes: [] }
        : { ok: true, clean: true };
      res.end(JSON.stringify({ data }));
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  port = server.address().port;
  home = fs.mkdtempSync(path.join(os.tmpdir(), "wh-outbox-home-"));
  repo = fs.mkdtempSync(path.join(os.tmpdir(), "wh-outbox-repo-"));
  const g = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  g("init", "-q", "-b", "main");
  g("config", "user.email", "t@example.com");
  g("config", "user.name", "T");
  fs.writeFileSync(path.join(repo, "wheelhouse.json"), JSON.stringify({ url: `http://127.0.0.1:${port}`, project: "QQ", defaultBranch: "main" }));
  fs.writeFileSync(path.join(repo, "a.txt"), "one\n");
  g("add", "-A");
  g("commit", "-q", "-m", "first");
});
after(() => {
  server.close();
  fs.rmSync(repo, { recursive: true, force: true });
  fs.rmSync(home, { recursive: true, force: true });
});

const env = (extra = {}) => ({ PATH: process.env.PATH, HOME: home, ...extra });
const outbox = () => (fs.existsSync(path.join(repo, ".wheelhouse", "outbox")) ? fs.readdirSync(path.join(repo, ".wheelhouse", "outbox")).filter((n) => n.endsWith(".json")) : []);
const commit = (file, text) => {
  fs.writeFileSync(path.join(repo, file), text);
  spawnSync("git", ["add", "-A"], { cwd: repo });
  spawnSync("git", ["commit", "-q", "-m", `edit ${file}`], { cwd: repo });
};
// The fake server lives in this process, so anything that talks to it must run asynchronously.
const run = (args, { input = "", extra = {} } = {}) =>
  new Promise((resolve) => {
    const p = spawn("node", [script, ...args], { cwd: repo, env: env(extra) });
    let stdout = "", stderr = "";
    p.stdout.on("data", (c) => (stdout += c));
    p.stderr.on("data", (c) => (stderr += c));
    p.on("close", (status) => resolve({ status, stdout, stderr }));
    p.stdin.end(input);
  });

test("a git hook with no key only queues the landing and never calls the server", async () => {
  commit("a.txt", "two\n");
  const before = calls.length;
  const r = await run(["land", "--git-hook", "post-commit"]);
  assert.equal(r.status, 0);
  assert.equal(calls.length, before, "no request was made");
  const files = outbox();
  assert.equal(files.length, 1);
  const entry = JSON.parse(fs.readFileSync(path.join(repo, ".wheelhouse", "outbox", files[0]), "utf8"));
  assert.equal(entry.branch, "main");
  assert.deepEqual(Object.keys(entry.files), ["a.txt"]);
  assert.match(entry.files["a.txt"], /^[0-9a-f]{40}$/);
  assert.equal(entry.sha, spawnSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).stdout.trim());
});

test("the session-start hook, which has the plugin's key, flushes the queue and deletes what it sent", async () => {
  const r = await run(["hook", "session-start"], { input: JSON.stringify({ cwd: repo, session_id: "sess-F" }), extra: { CLAUDE_PLUGIN_OPTION_API_KEY: "wh_plugin", CLAUDE_PLUGIN_OPTION_SERVER_URL: `http://127.0.0.1:${port}` } });
  assert.equal(r.status, 0);
  const land = calls.filter((c) => c.tool === "land_changes");
  assert.equal(land.length >= 1, true);
  assert.equal(land[0].auth, "Bearer wh_plugin");
  assert.deepEqual(Object.keys(land[0].args.files), ["a.txt"]);
  assert.equal(land[0].args.session, "sess-F");
  assert.deepEqual(outbox(), [], "queue emptied");
});

test("a baseline left pending by a key-less init is sent by the next session start, quietly", async () => {
  const marker = path.join(repo, ".wheelhouse", "baseline-pending");
  fs.writeFileSync(marker, "x\n");
  const r = await run(["hook", "session-start"], { input: JSON.stringify({ cwd: repo, session_id: "sess-B" }), extra: { CLAUDE_PLUGIN_OPTION_API_KEY: "wh_plugin", CLAUDE_PLUGIN_OPTION_SERVER_URL: `http://127.0.0.1:${port}` } });
  assert.equal(r.status, 0);
  assert.ok(calls.some((c) => c.tool === "land_changes" && c.args.baseline === true), "baseline sent");
  assert.equal(fs.existsSync(marker), false, "marker cleared");
  assert.ok(!/recorded baseline/.test(r.stdout), "nothing leaks into the hook's stdout");
  assert.doesNotThrow(() => JSON.parse(r.stdout), "hook output is still valid JSON");
});

test("a failing server leaves the queue intact, and the next flush sends it", async () => {
  commit("b.txt", "x\n");
  await run(["land", "--git-hook", "post-commit"]);
  assert.equal(outbox().length, 1);
  landStatus = 500;
  await run(["land", "--flush"], { extra: { WHEELHOUSE_KEY: "wh_k" } });
  assert.equal(outbox().length, 1, "kept after a 500");
  landStatus = 200;
  const r = await run(["land", "--flush"], { extra: { WHEELHOUSE_KEY: "wh_k" } });
  assert.match(r.stdout, /sent 1 of 1/);
  assert.deepEqual(outbox(), []);
});

test("a payload the server rejects (400) is dropped instead of blocking the queue forever", async () => {
  commit("c.txt", "y\n");
  await run(["land", "--git-hook", "post-commit"]);
  landStatus = 400;
  await run(["land", "--flush"], { extra: { WHEELHOUSE_KEY: "wh_k" } });
  landStatus = 200;
  assert.deepEqual(outbox(), []);
});

test("when the CLI also has a key, the git hook sends right away", async () => {
  commit("d.txt", "z\n");
  const before = calls.filter((c) => c.tool === "land_changes").length;
  await run(["land", "--git-hook", "post-commit"], { extra: { WHEELHOUSE_KEY: "wh_k" } });
  assert.equal(calls.filter((c) => c.tool === "land_changes").length, before + 1);
  assert.deepEqual(outbox(), []);
});

test("nothing is queued for a branch other than the default", async () => {
  spawnSync("git", ["checkout", "-q", "-b", "feature"], { cwd: repo });
  commit("e.txt", "w\n");
  await run(["land", "--git-hook", "post-commit"]);
  assert.deepEqual(outbox(), []);
  spawnSync("git", ["checkout", "-q", "main"], { cwd: repo });
});

test("status shows what is queued", async () => {
  commit("f.txt", "v\n");
  await run(["land", "--git-hook", "post-commit"]);
  const r = await run(["status"]);
  assert.match(r.stdout, /queued:\s+1 landing/);
});

test("a repo that was never linked is left alone (no queue, no error)", async () => {
  const bare = fs.mkdtempSync(path.join(os.tmpdir(), "wh-outbox-bare-"));
  spawnSync("git", ["init", "-q"], { cwd: bare });
  const r = spawnSync("node", [script, "land", "--git-hook", "post-commit"], { cwd: bare, env: env(), encoding: "utf8" });
  assert.equal(r.status, 0);
  assert.equal(fs.existsSync(path.join(bare, ".wheelhouse")), false);
  fs.rmSync(bare, { recursive: true, force: true });
});
