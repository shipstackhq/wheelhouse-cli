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

test("a worktree can start from another branch; a missing one is a clear error", async () => {
  const seed = path.join(tmp, "seed");
  g(seed, "checkout", "-q", "-b", "staging");
  fs.writeFileSync(path.join(seed, "STAGING.md"), "only on staging\n");
  g(seed, "add", ".");
  g(seed, "commit", "-m", "staging");
  g(seed, "push", origin, "staging");
  await ws.ensureClone(repository());
  const job = "55555555-5555-5555-5555-555555555555";
  const { dir, base } = await ws.createWorktree(repository(), job, "wh/WH-2-x", "staging");
  assert.equal(base, "origin/staging");
  assert.equal(fs.existsSync(path.join(dir, "STAGING.md")), true);
  await ws.removeWorktree(REPO, job);
  await assert.rejects(ws.createWorktree(repository(), job, "wh/WH-2-y", "nope"), /does not exist/);
  await assert.rejects(ws.createWorktree(repository(), job, "wh/WH-2-z", "../x"), /refusing base/);
});

test("a continuation starts from the job's own pushed branch and counts only what it adds", async () => {
  await ws.ensureClone(repository());
  const first = "66666666-6666-6666-6666-666666666666";
  const a = await ws.createWorktree(repository(), first, "wh/WH-3-cont", "main");
  fs.writeFileSync(path.join(a.dir, "one.txt"), "1\n");
  await ws.commitLeftovers(a.dir, "WH-3: one");
  await ws.push(a.dir, "wh/WH-3-cont");
  await ws.removeWorktree(REPO, first);
  await ws.ensureClone(repository()); // the next run fetches, as every run does

  const second = "77777777-7777-7777-7777-777777777777";
  const b = await ws.createWorktree(repository(), second, "wh/WH-3-cont", "wh/WH-3-cont");
  assert.equal(b.base, "origin/wh/WH-3-cont");
  assert.equal(fs.existsSync(path.join(b.dir, "one.txt")), true, "the earlier run's work is there");
  assert.equal((await ws.summary(b.dir, b.base)).ahead, 0, "nothing new yet");
  fs.writeFileSync(path.join(b.dir, "two.txt"), "2\n");
  await ws.commitLeftovers(b.dir, "WH-3: two");
  const sum = await ws.summary(b.dir, b.base);
  assert.deepEqual([sum.ahead, sum.diffStats.files], [1, 1], "only this run's commit counts");
  await ws.push(b.dir, "wh/WH-3-cont");
  assert.equal(g(origin, "rev-list", "--count", "main..wh/WH-3-cont"), "2", "both runs' commits on one branch");
  await ws.removeWorktree(REPO, second);
});

test("a continuation brings in what landed on its base; a conflict is left for the agent", async () => {
  const seed = path.join(tmp, "seed");
  g(seed, "checkout", "-q", "main");
  await ws.ensureClone(repository());
  const job = "88888888-8888-8888-8888-888888888888";
  const { dir } = await ws.createWorktree(repository(), job, "wh/WH-4-sync", "main");
  assert.equal(await ws.syncBase(dir, "origin/main"), "current");

  fs.writeFileSync(path.join(seed, "landed.txt"), "from main\n");
  g(seed, "add", ".");
  g(seed, "commit", "-m", "landed");
  g(seed, "push", origin, "main");
  await ws.ensureClone(repository());
  assert.equal(await ws.syncBase(dir, "origin/main"), "merged");
  assert.equal(fs.existsSync(path.join(dir, "landed.txt")), true);

  fs.writeFileSync(path.join(dir, "landed.txt"), "from the branch\n");
  await ws.commitLeftovers(dir, "WH-4: mine");
  fs.writeFileSync(path.join(seed, "landed.txt"), "changed on main\n");
  g(seed, "commit", "-am", "again");
  g(seed, "push", origin, "main");
  await ws.ensureClone(repository());
  assert.deepEqual(await ws.syncBase(dir, "origin/main"), ["landed.txt"]);
  assert.match(fs.readFileSync(path.join(dir, "landed.txt"), "utf8"), /<<<<<<</);
  await ws.removeWorktree(REPO, job);
});

test("a branch name that is not the job's shape is refused", async () => {
  await assert.rejects(ws.createWorktree(repository(), JOB, "main"), /refusing branch/);
  await assert.rejects(ws.createWorktree(repository(), JOB, "wh/../../etc"), /refusing branch/);
  assert.throws(() => ws.repoDir("../../x"), /bad repository id/);
});

test("continuing a stopped run whose kept worktree still holds the branch: its leftovers are saved and the branch is picked up", async () => {
  await ws.ensureClone(repository());
  const first = "77777777-7777-7777-7777-777777777777";
  const second = "88888888-8888-8888-8888-888888888888";
  const a = await ws.createWorktree(repository(), first, "wh/WH-4-stopped", "main");
  fs.writeFileSync(path.join(a.dir, "half.txt"), "half done\n");
  const b = await ws.createWorktree(repository(), second, "wh/WH-4-stopped", "main");
  assert.equal(fs.existsSync(a.dir), false, "the stopped run's worktree is gone");
  assert.equal(fs.readFileSync(path.join(b.dir, "half.txt"), "utf8"), "half done\n");
  await ws.removeWorktree(REPO, second);
});

test("a chat worktree is detached, reused for the thread, and stays where the chat moved it", async () => {
  const thread = "99999999-9999-9999-9999-999999999999";
  await ws.ensureClone(repository());
  const a = await ws.chatWorktree(repository(), thread);
  assert.equal(a.dir, path.join(tmp, "home", "chats", REPO, thread));
  assert.equal(g(a.dir, "branch", "--show-current"), "", "detached: no branch");
  assert.equal(a.commit, g(origin, "rev-parse", "refs/heads/main"));
  assert.deepEqual(a.branches, ["origin/main"]);
  const seed = path.join(tmp, "seed");
  g(seed, "checkout", "-q", "main");
  fs.writeFileSync(path.join(seed, "later.md"), "later\n");
  g(seed, "add", ".");
  g(seed, "commit", "-m", "later");
  g(seed, "push", origin, "main");
  await ws.ensureClone(repository());
  const b = await ws.chatWorktree(repository(), thread);
  assert.equal(b.dir, a.dir, "the same folder");
  assert.equal(b.commit, a.commit, "not moved by the daemon");
  assert.deepEqual(b.branches, [], "no longer the tip of main");
  // The chat moves its own checkout, and the next message finds it there.
  g(b.dir, "checkout", "-q", "--detach", "origin/main");
  const c = await ws.chatWorktree(repository(), thread);
  assert.equal(c.commit, g(origin, "rev-parse", "refs/heads/main"));
  assert.deepEqual(c.branches, ["origin/main"]);
  assert.equal(fs.existsSync(path.join(c.dir, "later.md")), true);
  // A new thread starts from the default branch, which must exist.
  await assert.rejects(ws.chatWorktree({ ...repository(), defaultBranch: "nope" }, "88888888-8888-8888-8888-888888888888"), /does not exist/);
});

test("pruning removes chat worktrees unused for a day and keeps the recent ones", async () => {
  const old = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
  const fresh = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
  await ws.ensureClone(repository());
  const a = await ws.chatWorktree(repository(), old);
  const b = await ws.chatWorktree(repository(), fresh);
  const twoDays = new Date(Date.now() - 48 * 3600_000);
  fs.utimesSync(a.dir, twoDays, twoDays);
  await ws.pruneOld(24);
  assert.equal(fs.existsSync(a.dir), false);
  assert.equal(fs.existsSync(b.dir), true);
});
