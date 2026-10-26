// Clones and worktrees. The daemon never looks for repositories on the disk: it clones each one into its own folder the first time a
// job needs it (with the user's own git credentials), keeps it, and gives every job a fresh worktree on the job's branch from the
// repository's default branch, and every chat thread a detached one on that branch's tip. The user's own checkouts are never touched.

import { execFile } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

export class GitError extends Error {}

function run(cmd, args, opts = {}) {
  return new Promise((resolve, reject) => {
    execFile(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, timeout: opts.timeout ?? 10 * 60_000, ...opts, env: { ...process.env, GIT_TERMINAL_PROMPT: "0", ...(opts.env ?? {}) } }, (err, stdout, stderr) => {
      if (err) reject(new GitError(`${cmd} ${args.join(" ")}: ${(stderr || err.message).trim().split("\n").slice(-3).join(" ")}`));
      else resolve(stdout.trim());
    });
  });
}
const git = (cwd, ...args) => run("git", args, { cwd });

/** The URL to clone: the offer's https URL, or the SSH form when the user's config prefers it. */
export function cloneUrlFor(repository, protocol = "https") {
  if (protocol === "ssh" && repository.fullName) return `git@github.com:${repository.fullName}.git`;
  return repository.cloneUrl;
}

const SAFE_ID = /^[0-9a-f-]{36}$/;
const SAFE_BRANCH = /^wh\/[A-Za-z0-9._\/-]+$/;

export class Workspace {
  constructor(p, { protocol = "https", log = () => {} } = {}) {
    this.p = p;
    this.protocol = protocol;
    this.log = log;
    this.locks = new Map(); // one clone or fetch at a time per repository
  }

  repoDir(repositoryId) {
    if (!SAFE_ID.test(repositoryId)) throw new GitError(`bad repository id ${repositoryId}`);
    return path.join(this.p.repos, `${repositoryId}.git`);
  }
  worktreeDir(repositoryId, jobId, root = this.p.worktrees) {
    if (!SAFE_ID.test(repositoryId) || !SAFE_ID.test(jobId)) throw new GitError("bad id");
    return path.join(root, repositoryId, jobId);
  }

  /** The repository ids this machine already holds a clone of. */
  held() {
    try {
      return fs
        .readdirSync(this.p.repos)
        .filter((f) => f.endsWith(".git"))
        .map((f) => f.slice(0, -4))
        .filter((id) => SAFE_ID.test(id));
    } catch {
      return [];
    }
  }

  async locked(key, fn) {
    const before = this.locks.get(key) ?? Promise.resolve();
    const mine = before.then(fn, fn);
    this.locks.set(key, mine.catch(() => {}));
    return mine;
  }

  /** Clone the repository the first time, fetch it every time after. */
  async ensureClone(repository, onPhase = () => {}) {
    const dir = this.repoDir(repository.id);
    return this.locked(repository.id, async () => {
      if (!fs.existsSync(path.join(dir, "HEAD"))) {
        onPhase("cloning", repository.fullName ?? repository.name);
        fs.mkdirSync(this.p.repos, { recursive: true });
        const tmp = `${dir}.partial-${process.pid}`;
        fs.rmSync(tmp, { recursive: true, force: true });
        await run("git", ["clone", "--bare", cloneUrlFor(repository, this.protocol), tmp], { timeout: 30 * 60_000 });
        // A bare clone maps branches straight to refs/heads; fetch them as origin/* instead, like a normal clone.
        await git(tmp, "config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*");
        fs.renameSync(tmp, dir);
      } else onPhase("fetching", repository.fullName ?? repository.name);
      await run("git", ["fetch", "--prune", "origin"], { cwd: dir, timeout: 15 * 60_000 });
      return dir;
    });
  }

  /**
   * A new worktree for the job, on its branch, from origin/<from>: the branch the run was told to start from (for a continuation, the
   * job's own branch as the earlier run pushed it), else the repository's default. `base` in the answer is that start point, so the
   * summary and the push count only what this run added.
   */
  async createWorktree(repository, jobId, branch, baseBranch) {
    if (!SAFE_BRANCH.test(branch) || branch.includes("..")) throw new GitError(`refusing branch name ${branch}`);
    const repo = this.repoDir(repository.id);
    const dir = this.worktreeDir(repository.id, jobId);
    fs.mkdirSync(path.dirname(dir), { recursive: true });
    const from = baseBranch || repository.defaultBranch || "main";
    if (!/^[A-Za-z0-9._\/-]+$/.test(from) || from.startsWith("-") || from.includes("..")) throw new GitError(`refusing base branch ${from}`);
    const base = `origin/${from}`;
    try {
      await git(repo, "rev-parse", "--verify", "--quiet", `${base}^{commit}`);
    } catch {
      throw new GitError(`the branch ${from} does not exist in ${repository.fullName ?? repository.name}`);
    }
    // Continuing a stopped or lost run: its kept worktree still has the branch checked out, and git will not check one branch out twice.
    // Save what it left, free the branch, and pick it up where it was (its commits may never have been pushed).
    const holder = await this.worktreeHolding(repo, branch);
    if (holder) {
      await this.commitLeftovers(holder, `${branch}: work left by the stopped run`).catch(() => {});
      await git(repo, "worktree", "remove", "--force", holder).catch(async () => {
        fs.rmSync(holder, { recursive: true, force: true });
        await git(repo, "worktree", "prune").catch(() => {});
      });
      await git(repo, "worktree", "add", dir, branch);
      return { dir, base };
    }
    await git(repo, "worktree", "add", "-B", branch, dir, base);
    return { dir, base };
  }

  /**
   * The chat thread's worktree: detached (no branch, so nothing a job uses), made on first use at the tip of origin/<default branch>.
   * After that it stays where it is, since the chat may have moved it to another branch (`git checkout --detach origin/<branch>`) and
   * should find it there on the next message. Its folder is touched each time, so it is pruned only when the thread goes unused.
   * Answers `{ dir, commit, branches }` (the origin/… branches whose tip it is on); the caller has fetched (ensureClone).
   */
  async chatWorktree(repository, threadId) {
    const repo = this.repoDir(repository.id);
    const dir = this.worktreeDir(repository.id, threadId, this.p.chats);
    if (!fs.existsSync(path.join(dir, ".git"))) {
      const from = repository.defaultBranch || "main";
      if (!/^[A-Za-z0-9._\/-]+$/.test(from) || from.startsWith("-") || from.includes("..")) throw new GitError(`refusing base branch ${from}`);
      const base = `origin/${from}`;
      try {
        await git(repo, "rev-parse", "--verify", "--quiet", `${base}^{commit}`);
      } catch {
        throw new GitError(`the branch ${from} does not exist in ${repository.fullName ?? repository.name}`);
      }
      fs.rmSync(dir, { recursive: true, force: true });
      fs.mkdirSync(path.dirname(dir), { recursive: true });
      await git(repo, "worktree", "prune").catch(() => {});
      await git(repo, "worktree", "add", "--detach", dir, base);
    }
    const now = new Date();
    fs.utimesSync(dir, now, now);
    const branches = (await git(dir, "branch", "-r", "--points-at", "HEAD", "--format=%(refname)"))
      .split("\n")
      .filter((r) => r.startsWith("refs/remotes/") && !r.endsWith("/HEAD"))
      .map((r) => r.slice("refs/remotes/".length));
    return { dir, commit: await git(dir, "rev-parse", "HEAD"), branches };
  }

  /** The folder of another worktree of this clone that has `branch` checked out, or null. */
  async worktreeHolding(repo, branch) {
    const list = await git(repo, "worktree", "list", "--porcelain");
    for (const block of list.split(/\n\n+/)) {
      const dir = /^worktree (.+)$/m.exec(block)?.[1];
      if (dir && new RegExp(`^branch refs/heads/${branch.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`, "m").test(block) && path.resolve(dir) !== path.resolve(repo)) return dir;
    }
    return null;
  }

  /** Commit what the agent left uncommitted, so nothing is lost when the worktree goes. False when there was nothing. */
  async commitLeftovers(dir, message) {
    if (!(await git(dir, "status", "--porcelain"))) return false;
    await git(dir, "add", "-A");
    // The user's own identity when git has one, as for the agent's own commits; a placeholder otherwise, so the commit cannot fail.
    const known = await git(dir, "config", "user.email").catch(() => "");
    const who = known ? [] : ["-c", "user.name=Wheelhouse agent", "-c", "user.email=agent@wheelhouse.invalid"];
    await git(dir, ...who, "commit", "-m", message);
    return true;
  }

  async summary(dir, base) {
    const ahead = Number(await git(dir, "rev-list", "--count", `${base}..HEAD`)) || 0;
    const commit = await git(dir, "rev-parse", "HEAD");
    const stat = ahead ? await git(dir, "diff", "--shortstat", `${base}...HEAD`) : "";
    const n = (re) => Number(re.exec(stat)?.[1] ?? 0);
    return { ahead, commit, diffStats: { files: n(/(\d+) files? changed/), insertions: n(/(\d+) insertions?/), deletions: n(/(\d+) deletions?/) } };
  }

  async push(dir, branch) {
    await run("git", ["push", "--force-with-lease", "origin", `HEAD:refs/heads/${branch}`], { cwd: dir, timeout: 10 * 60_000 });
  }

  async removeWorktree(repositoryId, jobId, root) {
    const dir = this.worktreeDir(repositoryId, jobId, root);
    try {
      await git(this.repoDir(repositoryId), "worktree", "remove", "--force", dir);
    } catch {
      fs.rmSync(dir, { recursive: true, force: true });
      await git(this.repoDir(repositoryId), "worktree", "prune").catch(() => {});
    }
  }

  /** Remove worktrees kept from failed runs once they are older than `hours`, and chat worktrees unused for `chatHours`. */
  async pruneOld(hours, chatHours = 24) {
    for (const [root, age] of [[this.p.worktrees, hours], [this.p.chats, chatHours]]) {
      const cutoff = Date.now() - age * 3600_000;
      for (const repo of this.held()) {
        let entries = [];
        try {
          entries = fs.readdirSync(path.join(root, repo));
        } catch {
          continue;
        }
        for (const id of entries) {
          try {
            if (fs.statSync(path.join(root, repo, id)).mtimeMs < cutoff) await this.removeWorktree(repo, id, root);
          } catch (e) {
            this.log(`could not prune worktree ${repo}/${id}: ${e.message}`);
          }
        }
      }
    }
  }
}

/** Run a shell command (a repository's setup step) in a worktree. */
export function runSetup(cwd, command, signal) {
  return run("/bin/sh", ["-c", command], { cwd, signal, timeout: 20 * 60_000 });
}
