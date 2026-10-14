import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "bin", "wheelhouse.mjs");
let home, repo, server, url;

before(async () => {
  server = http.createServer((req, res) => {
    const ok = req.headers.authorization === "Bearer wh_good";
    res.statusCode = ok ? 200 : 401;
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify(ok ? { data: [{ key: "QQ" }] } : { error: { message: "bad key" } }));
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  url = `http://127.0.0.1:${server.address().port}`;
  home = fs.mkdtempSync(path.join(os.tmpdir(), "wh-login-home-"));
  repo = fs.mkdtempSync(path.join(os.tmpdir(), "wh-login-repo-"));
  spawnSync("git", ["init", "-q"], { cwd: repo });
});
after(() => {
  server.close();
  fs.rmSync(home, { recursive: true, force: true });
  fs.rmSync(repo, { recursive: true, force: true });
});

const run = (args, env = {}) =>
  spawnSync("node", [script, ...args], { cwd: repo, input: "", encoding: "utf8", env: { PATH: process.env.PATH, HOME: home, ...env } });
// The fake server lives in this process, so anything that talks to it must not block the event loop.
const runAsync = (args, env = {}) =>
  new Promise((resolve) => {
    const p = spawn("node", [script, ...args], { cwd: repo, env: { PATH: process.env.PATH, HOME: home, ...env } });
    let stdout = "", stderr = "";
    p.stdout.on("data", (c) => (stdout += c));
    p.stderr.on("data", (c) => (stderr += c));
    p.stdin.end();
    p.on("close", (status) => resolve({ status, stdout, stderr }));
  });
const credFile = () => path.join(home, ".config", "wheelhouse", "credentials.json");

test("scripted login (--url and --key) saves the key with mode 0600", () => {
  const r = run(["login", "--url", `${url}/`, "--key", "wh_good"]);
  assert.equal(r.status, 0);
  assert.deepEqual(JSON.parse(fs.readFileSync(credFile(), "utf8")), { [url]: "wh_good" });
  assert.equal(fs.statSync(credFile()).mode & 0o777, 0o600);
});

test("tightens a credentials file that was left world-readable", () => {
  fs.chmodSync(credFile(), 0o644);
  run(["login", "--url", url, "--key", "wh_other"]);
  assert.equal(fs.statSync(credFile()).mode & 0o777, 0o600);
});

test("login without a terminal explains how to script it and exits non-zero", () => {
  const r = run(["login"]);
  assert.equal(r.status, 1);
  assert.match(r.stderr, /needs a terminal/);
  assert.match(r.stderr, /--key|WHEELHOUSE_KEY/);
});

test("init in a script with no key links the repo, does not prompt, and leaves the baseline for the plugin", async () => {
  fs.rmSync(credFile(), { force: true });
  const fresh = fs.mkdtempSync(path.join(os.tmpdir(), "wh-login-nokey-"));
  spawnSync("git", ["init", "-q"], { cwd: fresh });
  const r = spawnSync("node", [script, "init", "--url", url, "--project", "QQ"], { cwd: fresh, input: "", encoding: "utf8", env: { PATH: process.env.PATH, HOME: home } });
  assert.equal(r.status, 0);
  assert.match(r.stderr, /No API key on this machine/);
  assert.match(r.stderr, /queue commits locally/);
  assert.ok(fs.existsSync(path.join(fresh, "wheelhouse.json")));
  assert.ok(fs.existsSync(path.join(fresh, ".wheelhouse", "baseline-pending")));
  assert.ok(fs.existsSync(path.join(fresh, ".git", "hooks", "post-commit")));
  fs.rmSync(fresh, { recursive: true, force: true });
});

test("init in a script still works with WHEELHOUSE_KEY", async () => {
  const r = await runAsync(["init", "--url", url, "--project", "QQ", "--no-git-hooks", "--no-baseline"], { WHEELHOUSE_KEY: "wh_good" });
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stdout, /connected/);
});

test("the login URL prompt defaults to the hosted Wheelhouse and init no longer requires --url", () => {
  const src = fs.readFileSync(script, "utf8");
  assert.match(src, /DEFAULT_URL = "https:\/\/wheelhouse\.shipstack\.run"/);
  const fresh = fs.mkdtempSync(path.join(os.tmpdir(), "wh-login-fresh-"));
  spawnSync("git", ["init", "-q"], { cwd: fresh });
  const r = spawnSync("node", [script, "init"], { cwd: fresh, input: "", encoding: "utf8", env: { PATH: process.env.PATH, HOME: home } }); // no project: usage names the default
  fs.rmSync(fresh, { recursive: true, force: true });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /https:\/\/wheelhouse\.shipstack\.run/);
  assert.doesNotMatch(r.stderr, /--url <[^>]+> --project/);
});
