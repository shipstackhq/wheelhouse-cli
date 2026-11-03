//! Clones and worktrees. The daemon never looks for repositories on the disk: it clones each one into its own folder the first time a
//! job needs it (with the user's own git credentials), keeps it, and gives every job a fresh worktree on the job's branch from the
//! repository's default branch, and every chat thread a detached one on that branch's tip. The user's own checkouts are never touched.
//!
//! Port of `daemon/src/workspace.mjs`. Every git operation runs the `git` executable. The calls are synchronous: the connection layer
//! will run them off its async tasks. Per-repository locking is not needed yet, because nothing calls this concurrently.

use crate::config::Paths;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq)]
pub struct GitError(pub String);

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for GitError {}

fn err(msg: impl Into<String>) -> GitError {
    GitError(msg.into())
}

/// Run `git <args>` in `cwd`. On failure the error names the command and the last lines of its stderr.
pub fn git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| err(format!("git {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let lines: Vec<&str> = stderr.trim().lines().collect();
        let tail = lines[lines.len().saturating_sub(3)..].join(" ");
        let detail = if tail.is_empty() {
            out.status.to_string()
        } else {
            tail
        };
        return Err(err(format!("git {}: {detail}", args.join(" "))));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn safe_id() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[0-9a-f-]{36}$").unwrap())
}

fn safe_branch() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^wh/[A-Za-z0-9._/-]+$").unwrap())
}

/// A base branch name git can take as a start point (no option-looking names, no ranges).
fn plain_branch(name: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[A-Za-z0-9._/-]+$").unwrap())
        .is_match(name)
        && !name.starts_with('-')
        && !name.contains("..")
}

/// The repository as Wheelhouse describes it in an offer.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoRef {
    pub id: String,
    pub name: String,
    pub full_name: Option<String>,
    pub clone_url: String,
    pub default_branch: Option<String>,
}

impl RepoRef {
    /// The name people see: the full name when there is one.
    pub fn label(&self) -> &str {
        self.full_name.as_deref().unwrap_or(&self.name)
    }
}

/// The URL to clone: the offer's https URL, or the SSH form when the user's config prefers it.
pub fn clone_url_for(repo: &RepoRef, protocol: &str) -> String {
    match &repo.full_name {
        Some(full) if protocol == "ssh" => format!("git@github.com:{full}.git"),
        _ => repo.clone_url.clone(),
    }
}

/// A job's worktree and the commit it started from.
#[derive(Debug, Clone, PartialEq)]
pub struct Worktree {
    pub dir: PathBuf,
    pub base: String,
}

/// A chat thread's worktree: where it is, the commit it is on, and the origin branches whose tip that is.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatWorktree {
    pub dir: PathBuf,
    pub commit: String,
    pub branches: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub ahead: u64,
    pub commit: String,
    pub files: u64,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncResult {
    /// The base is already in the branch.
    Current,
    /// The base was merged in cleanly.
    Merged,
    /// These paths conflict; the merge is left in progress with markers for the agent to resolve.
    Conflicts(Vec<String>),
}

pub struct Workspace {
    pub paths: Paths,
    pub protocol: String,
}

impl Workspace {
    pub fn new(paths: Paths, protocol: impl Into<String>) -> Self {
        Workspace {
            paths,
            protocol: protocol.into(),
        }
    }

    pub fn repo_dir(&self, repository_id: &str) -> Result<PathBuf, GitError> {
        if !safe_id().is_match(repository_id) {
            return Err(err(format!("bad repository id {repository_id}")));
        }
        Ok(self.paths.repos.join(format!("{repository_id}.git")))
    }

    pub fn worktree_dir(
        &self,
        repository_id: &str,
        job_id: &str,
        root: &Path,
    ) -> Result<PathBuf, GitError> {
        if !safe_id().is_match(repository_id) || !safe_id().is_match(job_id) {
            return Err(err("bad id"));
        }
        Ok(root.join(repository_id).join(job_id))
    }

    /// The repository ids this machine already holds a clone of.
    pub fn held(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.paths.repos) else {
            return vec![];
        };
        let mut ids: Vec<String> = entries
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|n| n.strip_suffix(".git").map(str::to_string))
            .filter(|id| safe_id().is_match(id))
            .collect();
        ids.sort();
        ids
    }

    /// Clone the repository the first time, fetch it every time after. `on_phase` gets "cloning" or "fetching" and the label.
    pub fn ensure_clone(
        &self,
        repo: &RepoRef,
        mut on_phase: impl FnMut(&str, &str),
    ) -> Result<PathBuf, GitError> {
        let dir = self.repo_dir(&repo.id)?;
        if !dir.join("HEAD").exists() {
            on_phase("cloning", repo.label());
            std::fs::create_dir_all(&self.paths.repos).map_err(|e| err(e.to_string()))?;
            let tmp = PathBuf::from(format!("{}.partial-{}", dir.display(), std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let url = clone_url_for(repo, &self.protocol);
            git(
                Path::new("."),
                &["clone", "--bare", &url, &tmp.display().to_string()],
            )?;
            // A bare clone maps branches straight to refs/heads; fetch them as origin/* instead, like a normal clone.
            git(
                &tmp,
                &[
                    "config",
                    "remote.origin.fetch",
                    "+refs/heads/*:refs/remotes/origin/*",
                ],
            )?;
            std::fs::rename(&tmp, &dir).map_err(|e| err(e.to_string()))?;
        } else {
            on_phase("fetching", repo.label());
        }
        git(&dir, &["fetch", "--prune", "origin"])?;
        Ok(dir)
    }

    /// A new worktree for the job, on its branch, from origin/<base>: the base the run was told to start from (for a continuation, the
    /// job's own branch as the earlier run pushed it), else the repository's default. `base` in the answer is that start point, so the
    /// summary and the push count only what this run added.
    pub fn create_worktree(
        &self,
        repo: &RepoRef,
        job_id: &str,
        branch: &str,
        base_branch: Option<&str>,
    ) -> Result<Worktree, GitError> {
        if !safe_branch().is_match(branch) || branch.contains("..") {
            return Err(err(format!("refusing branch name {branch}")));
        }
        let repo_dir = self.repo_dir(&repo.id)?;
        let dir = self.worktree_dir(&repo.id, job_id, &self.paths.worktrees)?;
        std::fs::create_dir_all(dir.parent().unwrap_or(Path::new(".")))
            .map_err(|e| err(e.to_string()))?;
        let from = base_branch
            .filter(|b| !b.is_empty())
            .or(repo.default_branch.as_deref())
            .unwrap_or("main");
        if !plain_branch(from) {
            return Err(err(format!("refusing base branch {from}")));
        }
        let base = format!("origin/{from}");
        self.check_start_point(&repo_dir, &base, repo.label())?;

        // Continuing a stopped or lost run: its kept worktree still has the branch checked out, and git will not check one branch out
        // twice. Save what it left, free the branch, and pick it up where it was (its commits may never have been pushed).
        if let Some(holder) = self.worktree_holding(&repo_dir, branch)? {
            let _ =
                self.commit_leftovers(&holder, &format!("{branch}: work left by the stopped run"));
            if git(
                &repo_dir,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    &holder.display().to_string(),
                ],
            )
            .is_err()
            {
                let _ = std::fs::remove_dir_all(&holder);
                let _ = git(&repo_dir, &["worktree", "prune"]);
            }
            git(
                &repo_dir,
                &["worktree", "add", &dir.display().to_string(), branch],
            )?;
            return Ok(Worktree { dir, base });
        }
        git(
            &repo_dir,
            &[
                "worktree",
                "add",
                "-B",
                branch,
                &dir.display().to_string(),
                &base,
            ],
        )?;
        Ok(Worktree { dir, base })
    }

    fn check_start_point(&self, repo_dir: &Path, base: &str, label: &str) -> Result<(), GitError> {
        git(
            repo_dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{base}^{{commit}}"),
            ],
        )
        .map(|_| ())
        .map_err(|_| {
            err(format!(
                "the branch {} does not exist in {label}",
                base.trim_start_matches("origin/")
            ))
        })
    }

    /// The chat thread's worktree: detached (no branch, so nothing a job uses), made on first use at the tip of origin/<default branch>.
    /// After that it stays where it is, since the chat may have moved it and should find it there on the next message. Its folder is
    /// touched each time, so it is pruned only when the thread goes unused. The caller has fetched (ensure_clone).
    pub fn chat_worktree(&self, repo: &RepoRef, thread_id: &str) -> Result<ChatWorktree, GitError> {
        let repo_dir = self.repo_dir(&repo.id)?;
        let dir = self.worktree_dir(&repo.id, thread_id, &self.paths.chats)?;
        if !dir.join(".git").exists() {
            let from = repo
                .default_branch
                .as_deref()
                .filter(|b| !b.is_empty())
                .unwrap_or("main");
            if !plain_branch(from) {
                return Err(err(format!("refusing base branch {from}")));
            }
            let base = format!("origin/{from}");
            self.check_start_point(&repo_dir, &base, repo.label())?;
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.parent().unwrap_or(Path::new(".")))
                .map_err(|e| err(e.to_string()))?;
            let _ = git(&repo_dir, &["worktree", "prune"]);
            git(
                &repo_dir,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    &dir.display().to_string(),
                    &base,
                ],
            )?;
        }
        // Touch the folder so pruning sees the thread as used.
        if let Ok(f) = std::fs::File::open(&dir) {
            let _ = f.set_modified(SystemTime::now());
        }
        let refs = git(
            &dir,
            &["branch", "-r", "--points-at", "HEAD", "--format=%(refname)"],
        )?;
        let branches = refs
            .lines()
            .filter_map(|r| r.strip_prefix("refs/remotes/"))
            .filter(|r| !r.ends_with("/HEAD"))
            .map(str::to_string)
            .collect();
        Ok(ChatWorktree {
            commit: git(&dir, &["rev-parse", "HEAD"])?,
            dir,
            branches,
        })
    }

    /// The folder of another worktree of this clone that has `branch` checked out, or None.
    pub fn worktree_holding(
        &self,
        repo_dir: &Path,
        branch: &str,
    ) -> Result<Option<PathBuf>, GitError> {
        let list = git(repo_dir, &["worktree", "list", "--porcelain"])?;
        let wanted = format!("branch refs/heads/{branch}");
        for block in list.split("\n\n") {
            let dir = block.lines().find_map(|l| l.strip_prefix("worktree "));
            let holds = block.lines().any(|l| l == wanted);
            if let Some(dir) = dir {
                if holds && Path::new(dir) != repo_dir {
                    return Ok(Some(PathBuf::from(dir)));
                }
            }
        }
        Ok(None)
    }

    /// The identity commits use: the user's own when git has one, else a placeholder, so a commit cannot fail for want of one.
    fn identity_args(dir: &Path) -> Vec<&'static str> {
        let known = git(dir, &["config", "user.email"])
            .map(|e| !e.is_empty())
            .unwrap_or(false);
        if known {
            vec![]
        } else {
            vec![
                "-c",
                "user.name=Wheelhouse agent",
                "-c",
                "user.email=agent@wheelhouse.invalid",
            ]
        }
    }

    /// Commit what the agent left uncommitted, so nothing is lost when the worktree goes. False when there was nothing.
    pub fn commit_leftovers(&self, dir: &Path, message: &str) -> Result<bool, GitError> {
        if git(dir, &["status", "--porcelain"])?.is_empty() {
            return Ok(false);
        }
        git(dir, &["add", "-A"])?;
        let mut args: Vec<&str> = Self::identity_args(dir);
        args.extend(["commit", "-m", message]);
        git(dir, &args)?;
        Ok(true)
    }

    /// A continuation brings its base in first, so the agent works on what has landed since (the agent itself may not merge or fetch).
    pub fn sync_base(&self, dir: &Path, base: &str) -> Result<SyncResult, GitError> {
        if git(dir, &["merge-base", "--is-ancestor", base, "HEAD"]).is_ok() {
            return Ok(SyncResult::Current);
        }
        let mut args: Vec<&str> = Self::identity_args(dir);
        args.extend(["merge", "--no-edit", base]);
        match git(dir, &args) {
            Ok(_) => Ok(SyncResult::Merged),
            Err(_) => {
                let files =
                    git(dir, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
                Ok(SyncResult::Conflicts(
                    files
                        .lines()
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .collect(),
                ))
            }
        }
    }

    /// The commit the worktree is on.
    pub fn head(&self, dir: &Path) -> Result<String, GitError> {
        git(dir, &["rev-parse", "HEAD"])
    }

    pub fn summary(&self, dir: &Path, base: &str) -> Result<Summary, GitError> {
        let ahead: u64 = git(dir, &["rev-list", "--count", &format!("{base}..HEAD")])?
            .parse()
            .unwrap_or(0);
        let commit = git(dir, &["rev-parse", "HEAD"])?;
        let stat = if ahead > 0 {
            git(dir, &["diff", "--shortstat", &format!("{base}...HEAD")])?
        } else {
            String::new()
        };
        let n = |pat: &str| -> u64 {
            Regex::new(pat)
                .ok()
                .and_then(|re| re.captures(&stat).and_then(|c| c[1].parse().ok()))
                .unwrap_or(0)
        };
        Ok(Summary {
            ahead,
            commit,
            files: n(r"(\d+) files? changed"),
            insertions: n(r"(\d+) insertions?"),
            deletions: n(r"(\d+) deletions?"),
        })
    }

    pub fn push(&self, dir: &Path, branch: &str) -> Result<(), GitError> {
        git(
            dir,
            &[
                "push",
                "--force-with-lease",
                "origin",
                &format!("HEAD:refs/heads/{branch}"),
            ],
        )
        .map(|_| ())
    }

    pub fn remove_worktree(
        &self,
        repository_id: &str,
        job_id: &str,
        root: &Path,
    ) -> Result<(), GitError> {
        let dir = self.worktree_dir(repository_id, job_id, root)?;
        let repo_dir = self.repo_dir(repository_id)?;
        if git(
            &repo_dir,
            &["worktree", "remove", "--force", &dir.display().to_string()],
        )
        .is_err()
        {
            let _ = std::fs::remove_dir_all(&dir);
            let _ = git(&repo_dir, &["worktree", "prune"]);
        }
        Ok(())
    }

    /// Remove worktrees kept from failed runs once they are older than `hours`, and chat worktrees unused for `chat_hours`.
    /// Returns what could not be removed, for the log.
    pub fn prune_old(&self, hours: u64, chat_hours: u64) -> Vec<String> {
        let mut problems = vec![];
        let now = SystemTime::now();
        for (root, age) in [
            (self.paths.worktrees.clone(), hours),
            (self.paths.chats.clone(), chat_hours),
        ] {
            for repo in self.held() {
                let Ok(entries) = std::fs::read_dir(root.join(&repo)) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let old = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .map(|t| {
                            now.duration_since(t)
                                .map(|d| d.as_secs() >= age * 3600)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    if !old {
                        continue;
                    }
                    let id = entry.file_name().to_string_lossy().to_string();
                    if let Err(e) = self.remove_worktree(&repo, &id, &root) {
                        problems.push(format!("could not prune worktree {repo}/{id}: {e}"));
                    }
                }
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const REPO: &str = "33333333-3333-3333-3333-333333333333";
    const JOB: &str = "44444444-4444-4444-4444-444444444444";

    /// A git command with a fixed author, for the test repositories.
    fn g(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .envs([
                ("GIT_AUTHOR_NAME", "t"),
                ("GIT_AUTHOR_EMAIL", "t@e"),
                ("GIT_COMMITTER_NAME", "t"),
                ("GIT_COMMITTER_EMAIL", "t@e"),
            ])
            .output()
            .expect("git on PATH");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A real "GitHub": a bare repository on disk with one commit on main, and a workspace whose home is inside the same folder.
    struct Fixture {
        tmp: PathBuf,
        origin: PathBuf,
        seed: PathBuf,
        ws: Workspace,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let tmp = std::env::temp_dir().join(format!("wh-ws-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&tmp);
            fs::create_dir_all(&tmp).unwrap();
            let origin = tmp.join("origin.git");
            let seed = tmp.join("seed");
            g(
                &tmp,
                &[
                    "init",
                    "--bare",
                    "-b",
                    "main",
                    &origin.display().to_string(),
                ],
            );
            g(&tmp, &["init", "-b", "main", &seed.display().to_string()]);
            fs::write(seed.join("README.md"), "hello\n").unwrap();
            g(&seed, &["add", "."]);
            g(&seed, &["commit", "-m", "first"]);
            g(&seed, &["push", &origin.display().to_string(), "main"]);
            let ws = Workspace::new(Paths::new(tmp.join("home")), "https");
            Fixture {
                tmp,
                origin,
                seed,
                ws,
            }
        }

        fn origin(&self) -> String {
            self.origin.display().to_string()
        }

        fn repo(&self) -> RepoRef {
            RepoRef {
                id: REPO.into(),
                name: "app".into(),
                full_name: Some("acme/app".into()),
                clone_url: self.origin(),
                default_branch: Some("main".into()),
            }
        }

        /// Add a commit to the seed (the "GitHub" side) and push it to origin.
        fn land(&self, file: &str, content: &str, message: &str, branch: &str) {
            if let Some(parent) = self.seed.join(file).parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(self.seed.join(file), content).unwrap();
            g(&self.seed, &["add", "."]);
            g(&self.seed, &["commit", "-m", message]);
            g(&self.seed, &["push", &self.origin(), branch]);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.tmp);
        }
    }

    #[test]
    fn clone_once_worktree_on_the_jobs_branch_commit_what_was_left_push_and_remove() {
        let f = Fixture::new("basic");
        let repo = f.repo();
        let mut phases = vec![];
        f.ws.ensure_clone(&repo, |p, _| phases.push(p.to_string()))
            .unwrap();
        f.ws.ensure_clone(&repo, |p, _| phases.push(p.to_string()))
            .unwrap();
        assert_eq!(phases, ["cloning", "fetching"]);
        assert_eq!(f.ws.held(), [REPO]);

        let wt =
            f.ws.create_worktree(&repo, JOB, "wh/WH-1-readme", None)
                .unwrap();
        assert_eq!(wt.base, "origin/main");
        assert_eq!(g(&wt.dir, &["branch", "--show-current"]), "wh/WH-1-readme");
        assert_eq!(f.ws.summary(&wt.dir, &wt.base).unwrap().ahead, 0);

        fs::write(wt.dir.join("README.md"), "hello\nmore\n").unwrap();
        fs::write(wt.dir.join("new.txt"), "x\n").unwrap();
        assert!(f.ws.commit_leftovers(&wt.dir, "WH-1: leftovers").unwrap());
        assert!(!f.ws.commit_leftovers(&wt.dir, "WH-1: nothing").unwrap());
        let sum = f.ws.summary(&wt.dir, &wt.base).unwrap();
        assert_eq!(sum.ahead, 1);
        assert_eq!((sum.files, sum.insertions, sum.deletions), (2, 2, 0));
        assert_eq!(f.ws.head(&wt.dir).unwrap(), sum.commit);

        f.ws.push(&wt.dir, "wh/WH-1-readme").unwrap();
        assert_eq!(
            g(&f.origin, &["rev-parse", "refs/heads/wh/WH-1-readme"]),
            sum.commit
        );
        assert_ne!(
            g(&f.origin, &["rev-parse", "refs/heads/main"]),
            sum.commit,
            "main is untouched"
        );
        f.ws.remove_worktree(REPO, JOB, &f.ws.paths.worktrees.clone())
            .unwrap();
        assert!(!wt.dir.exists());
    }

    #[test]
    fn a_worktree_can_start_from_another_branch_and_a_missing_one_is_a_clear_error() {
        let f = Fixture::new("base");
        let repo = f.repo();
        g(&f.seed, &["checkout", "-q", "-b", "staging"]);
        f.land("STAGING.md", "only on staging\n", "staging", "staging");
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        let job = "55555555-5555-5555-5555-555555555555";
        let wt =
            f.ws.create_worktree(&repo, job, "wh/WH-2-x", Some("staging"))
                .unwrap();
        assert_eq!(wt.base, "origin/staging");
        assert!(wt.dir.join("STAGING.md").exists());
        f.ws.remove_worktree(REPO, job, &f.ws.paths.worktrees.clone())
            .unwrap();
        let e =
            f.ws.create_worktree(&repo, job, "wh/WH-2-y", Some("nope"))
                .unwrap_err();
        assert!(e.0.contains("does not exist"), "{e}");
        let e =
            f.ws.create_worktree(&repo, job, "wh/WH-2-z", Some("../x"))
                .unwrap_err();
        assert!(e.0.contains("refusing base"), "{e}");
    }

    #[test]
    fn a_continuation_starts_from_the_jobs_own_pushed_branch_and_counts_only_what_it_adds() {
        let f = Fixture::new("cont");
        let repo = f.repo();
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        let first = "66666666-6666-6666-6666-666666666666";
        let a =
            f.ws.create_worktree(&repo, first, "wh/WH-3-cont", Some("main"))
                .unwrap();
        fs::write(a.dir.join("one.txt"), "1\n").unwrap();
        f.ws.commit_leftovers(&a.dir, "WH-3: one").unwrap();
        f.ws.push(&a.dir, "wh/WH-3-cont").unwrap();
        f.ws.remove_worktree(REPO, first, &f.ws.paths.worktrees.clone())
            .unwrap();
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();

        let second = "77777777-7777-7777-7777-777777777777";
        let b =
            f.ws.create_worktree(&repo, second, "wh/WH-3-cont", Some("wh/WH-3-cont"))
                .unwrap();
        assert_eq!(b.base, "origin/wh/WH-3-cont");
        assert!(
            b.dir.join("one.txt").exists(),
            "the earlier run's work is there"
        );
        assert_eq!(f.ws.summary(&b.dir, &b.base).unwrap().ahead, 0);
        fs::write(b.dir.join("two.txt"), "2\n").unwrap();
        f.ws.commit_leftovers(&b.dir, "WH-3: two").unwrap();
        let sum = f.ws.summary(&b.dir, &b.base).unwrap();
        assert_eq!(
            (sum.ahead, sum.files),
            (1, 1),
            "only this run's commit counts"
        );
        f.ws.push(&b.dir, "wh/WH-3-cont").unwrap();
        assert_eq!(
            g(&f.origin, &["rev-list", "--count", "main..wh/WH-3-cont"]),
            "2"
        );
        f.ws.remove_worktree(REPO, second, &f.ws.paths.worktrees.clone())
            .unwrap();
    }

    #[test]
    fn a_continuation_brings_in_what_landed_on_its_base_and_a_conflict_is_left_for_the_agent() {
        let f = Fixture::new("sync");
        let repo = f.repo();
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        let job = "88888888-8888-8888-8888-888888888888";
        let wt =
            f.ws.create_worktree(&repo, job, "wh/WH-4-sync", Some("main"))
                .unwrap();
        assert_eq!(
            f.ws.sync_base(&wt.dir, "origin/main").unwrap(),
            SyncResult::Current
        );

        f.land("landed.txt", "from main\n", "landed", "main");
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        assert_eq!(
            f.ws.sync_base(&wt.dir, "origin/main").unwrap(),
            SyncResult::Merged
        );
        assert!(wt.dir.join("landed.txt").exists());

        fs::write(wt.dir.join("landed.txt"), "from the branch\n").unwrap();
        f.ws.commit_leftovers(&wt.dir, "WH-4: mine").unwrap();
        f.land("landed.txt", "changed on main\n", "again", "main");
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        assert_eq!(
            f.ws.sync_base(&wt.dir, "origin/main").unwrap(),
            SyncResult::Conflicts(vec!["landed.txt".into()])
        );
        assert!(fs::read_to_string(wt.dir.join("landed.txt"))
            .unwrap()
            .contains("<<<<<<<"));
        f.ws.remove_worktree(REPO, job, &f.ws.paths.worktrees.clone())
            .unwrap();
    }

    #[test]
    fn a_branch_name_that_is_not_the_jobs_shape_is_refused() {
        let f = Fixture::new("names");
        let repo = f.repo();
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        assert!(f
            .ws
            .create_worktree(&repo, JOB, "main", None)
            .unwrap_err()
            .0
            .contains("refusing branch"));
        assert!(f
            .ws
            .create_worktree(&repo, JOB, "wh/../../etc", None)
            .unwrap_err()
            .0
            .contains("refusing branch"));
        assert!(f
            .ws
            .repo_dir("../../x")
            .unwrap_err()
            .0
            .contains("bad repository id"));
    }

    #[test]
    fn continuing_a_stopped_run_whose_worktree_still_holds_the_branch_picks_up_its_leftovers() {
        let f = Fixture::new("stopped");
        let repo = f.repo();
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        let first = "77777777-7777-7777-7777-777777777777";
        let second = "88888888-8888-8888-8888-888888888888";
        let a =
            f.ws.create_worktree(&repo, first, "wh/WH-4-stopped", Some("main"))
                .unwrap();
        fs::write(a.dir.join("half.txt"), "half done\n").unwrap();
        let b =
            f.ws.create_worktree(&repo, second, "wh/WH-4-stopped", Some("main"))
                .unwrap();
        assert!(!a.dir.exists(), "the stopped run's worktree is gone");
        assert_eq!(
            fs::read_to_string(b.dir.join("half.txt")).unwrap(),
            "half done\n"
        );
        f.ws.remove_worktree(REPO, second, &f.ws.paths.worktrees.clone())
            .unwrap();
    }

    #[test]
    fn a_chat_worktree_is_detached_reused_for_the_thread_and_stays_where_the_chat_moved_it() {
        let f = Fixture::new("chat");
        let repo = f.repo();
        let thread = "99999999-9999-9999-9999-999999999999";
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        let a = f.ws.chat_worktree(&repo, thread).unwrap();
        assert_eq!(a.dir, f.ws.paths.chats.join(REPO).join(thread));
        assert_eq!(
            g(&a.dir, &["branch", "--show-current"]),
            "",
            "detached: no branch"
        );
        assert_eq!(a.commit, g(&f.origin, &["rev-parse", "refs/heads/main"]));
        assert_eq!(a.branches, ["origin/main"]);

        f.land("later.md", "later\n", "later", "main");
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        let b = f.ws.chat_worktree(&repo, thread).unwrap();
        assert_eq!(b.dir, a.dir, "the same folder");
        assert_eq!(b.commit, a.commit, "not moved by the daemon");
        assert!(b.branches.is_empty(), "no longer the tip of main");
        g(&b.dir, &["checkout", "-q", "--detach", "origin/main"]);
        let c = f.ws.chat_worktree(&repo, thread).unwrap();
        assert_eq!(c.commit, g(&f.origin, &["rev-parse", "refs/heads/main"]));
        assert_eq!(c.branches, ["origin/main"]);
        assert!(c.dir.join("later.md").exists());

        let mut stale = repo.clone();
        stale.default_branch = Some("nope".into());
        assert!(f
            .ws
            .chat_worktree(&stale, "88888888-8888-8888-8888-888888888888")
            .unwrap_err()
            .0
            .contains("does not exist"));
    }

    #[test]
    fn pruning_removes_chat_worktrees_unused_for_a_day_and_keeps_the_recent_ones() {
        let f = Fixture::new("prune");
        let repo = f.repo();
        let old = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let fresh = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        f.ws.ensure_clone(&repo, |_, _| {}).unwrap();
        let a = f.ws.chat_worktree(&repo, old).unwrap();
        let b = f.ws.chat_worktree(&repo, fresh).unwrap();
        let two_days = SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        fs::File::open(&a.dir)
            .unwrap()
            .set_modified(two_days)
            .unwrap();
        assert!(f.ws.prune_old(24, 24).is_empty());
        assert!(!a.dir.exists());
        assert!(b.dir.exists());
    }

    #[test]
    fn json_helpers_for_the_repo_shape_are_consistent() {
        let repo = RepoRef {
            id: REPO.into(),
            name: "app".into(),
            full_name: None,
            clone_url: "https://x/y.git".into(),
            default_branch: None,
        };
        assert_eq!(repo.label(), "app");
        assert_eq!(
            clone_url_for(&repo, "ssh"),
            "https://x/y.git",
            "no full name, so no SSH form"
        );
        let full = RepoRef {
            full_name: Some("acme/app".into()),
            ..repo
        };
        assert_eq!(clone_url_for(&full, "ssh"), "git@github.com:acme/app.git");
        assert_eq!(clone_url_for(&full, "https"), "https://x/y.git");
    }
}
