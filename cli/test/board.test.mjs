import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "bin", "wheelhouse.mjs");
let home, repo, outside, server, url;
const calls = [];

before(async () => {
  server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      const tool = req.url.split("/").pop();
      const args = JSON.parse(body || "{}");
      calls.push({ tool, args });
      const data = {
        list_issues: { issues: [{ ref: "QQ-1", title: "First", status: "Todo", priority: "high" }], more: 0 },
        update_issue: { ref: args.ref, title: "Fix the login page!", status: "In Progress" },
        create_issue: { ref: "QQ-2", title: args.title },
        comment: { id: "c1" },
      }[tool];
      res.statusCode = data ? 200 : 500;
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify(data ? { data } : { error: { message: "boom" } }));
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  url = `http://127.0.0.1:${server.address().port}`;
  home = fs.mkdtempSync(path.join(os.tmpdir(), "wh-board-home-"));
  repo = fs.mkdtempSync(path.join(os.tmpdir(), "wh-board-repo-"));
  outside = fs.mkdtempSync(path.join(os.tmpdir(), "wh-board-outside-"));
  for (const args of [["init", "-q", "-b", "main"], ["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "first"]]) spawnSync("git", args, { cwd: repo });
});
after(() => {
  server.close();
  for (const d of [home, repo, outside]) fs.rmSync(d, { recursive: true, force: true });
});

const run = (args, cwd = repo, extra = {}) =>
  new Promise((resolve) => {
    const p = spawn("node", [script, ...args], { cwd, env: { PATH: process.env.PATH, HOME: home, WHEELHOUSE_KEYSTORE: "file", ...extra } });
    let stdout = "", stderr = "";
    p.stdout.on("data", (c) => (stdout += c));
    p.stderr.on("data", (c) => (stderr += c));
    p.on("close", (status) => resolve({ status, stdout, stderr }));
  });
const last = () => calls[calls.length - 1];

test("outside a linked repo the board says how to set it up and exits 1", async () => {
  const r = await run(["issues"], outside);
  assert.equal(r.status, 1);
  assert.match(r.stderr, /wheelhouse login/);
});

test("login remembers the server, so board commands work in any folder; the team comes from the ref", async () => {
  assert.equal((await run(["login", "--url", url, "--key", "wh_k"], outside)).status, 0);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(home, ".wheelhouse", "config.json"), "utf8")), { url });
  const r = await run(["done", "qq-1"], outside);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(last(), { tool: "update_issue", args: { team: "QQ", ref: "QQ-1", status: "review" } });
});

test("issues lists with filters; it needs a team, from flags, the repo or the default", async () => {
  assert.match((await run(["issues"], outside)).stderr, /--team/);
  const r = await run(["issues", "--mine", "--cycle", "current", "--team", "QQ"], outside);
  assert.match(r.stdout, /QQ-1 +Todo +high +First/);
  assert.deepEqual(last().args, { team: "QQ", limit: 50, assignee: "me", cycle: "current" });
});

test("start claims the issue, moves it to In Progress and switches to its branch", async () => {
  const r = await run(["start", "QQ-7"]);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(last().args, { team: "QQ", ref: "QQ-7", status: "in_progress", assignee: "me" });
  assert.equal(spawnSync("git", ["branch", "--show-current"], { cwd: repo, encoding: "utf8" }).stdout.trim(), "wh/QQ-7-fix-the-login-page");
  assert.match((await run(["start", "QQ-7"])).stdout, /on branch wh\/QQ-7-fix-the-login-page$/m);
});

test("comment and new send what was typed", async () => {
  await run(["comment", "QQ-1", "looks", "good"], outside);
  assert.deepEqual(last(), { tool: "comment", args: { team: "QQ", ref: "QQ-1", body: "looks good" } });
  const r = await run(["new", "Add a logout button", "--priority", "low", "--team", "QQ"], outside);
  assert.match(r.stdout, /created QQ-2: Add a logout button/);
  assert.deepEqual(last().args, { team: "QQ", title: "Add a logout button", priority: "low" });
});

test("a server error is printed and exits 1", async () => {
  const r = await run(["issue", "QQ-1"], outside);
  assert.equal(r.status, 1);
  assert.match(r.stderr, /get_issue/);
});
