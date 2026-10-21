import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { paths } from "../src/config.mjs";
import { Workspace } from "../src/workspace.mjs";

// A real "GitHub": a bare repository on disk with one commit on main, cloned by its path.
let tmp, origin, ws;
const REPO = "33333333-3333-3333-3333-333333333333";
const JOB = "44444444-4444-4444-4444-444444444444";
const g = (cwd, ...a) => execFileSync("git", a, { cwd, encoding: "utf8", env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@e", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@e" } }).trim();

before(() => {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "wh-daemon-ws-"));
  origin = path.join(tmp, "origin.git");
  const seed = path.join(tmp, "seed");
  g(tmp, "init", "--bare", "-b", "main", origin);
  g(tmp, "init", "-b", "main", seed);
  fs.writeFileSync(path.join(seed, "README.md"), "hello\n");
  g(seed, "add", ".");
  g(seed, "commit", "-m", "first");
  g(seed, "push", origin, "main");
  ws = new Workspace(paths(path.join(tmp, "home")));
});
after(() => fs.rmSync(tmp, { recursive: true, force: true }));

const repository = () => ({ id: REPO, name: "app", fullName: "acme/app", cloneUrl: origin, defaultBranch: "main" });

test("clone once, worktree on the job's branch, commit what was left, push, and remove", async () => {
  const phases = [];
  await ws.ensureClone(repository(), (p) => phases.push(p));
  await ws.ensureClone(repository(), (p) => phases.push(p));
  assert.deepEqual(phases, ["cloning", "fetching"]);
  assert.deepEqual(ws.held(), [REPO]);

  const { dir, base } = await ws.createWorktree(repository(), JOB, "wh/WH-1-readme");
  assert.equal(base, "origin/main");
  assert.equal(g(dir, "branch", "--show-current"), "wh/WH-1-readme");
  assert.equal((await ws.summary(dir, base)).ahead, 0);

  fs.appendFileSync(path.join(dir, "README.md"), "more\n");
  fs.writeFileSync(path.join(dir, "new.txt"), "x\n");
  assert.equal(await ws.commitLeftovers(dir, "WH-1: leftovers"), true);
  assert.equal(await ws.commitLeftovers(dir, "WH-1: nothing"), false);
  const sum = await ws.summary(dir, base);
  assert.equal(sum.ahead, 1);
  assert.deepEqual(sum.diffStats, { files: 2, insertions: 2, deletions: 0 });

  await ws.push(dir, "wh/WH-1-readme");
  assert.equal(g(origin, "rev-parse", "refs/heads/wh/WH-1-readme"), sum.commit);
  assert.equal(g(origin, "rev-parse", "refs/heads/main") !== sum.commit, true, "main is untouched");

  await ws.removeWorktree(REPO, JOB);
  assert.equal(fs.existsSync(dir), false);
});

test("a branch name that is not the job's shape is refused", async () => {
  await assert.rejects(ws.createWorktree(repository(), JOB, "main"), /refusing branch/);
  await assert.rejects(ws.createWorktree(repository(), JOB, "wh/../../etc"), /refusing branch/);
  assert.throws(() => ws.repoDir("../../x"), /bad repository id/);
});
