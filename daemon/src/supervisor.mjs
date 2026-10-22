// The pool: which jobs this machine is running, whether it can take another, and each job from offer to finish.
//
// Every message from Wheelhouse is checked before anything happens: the server's signature against the key pinned at pairing, that it
// names this device, and (for an offer) that it has not expired. Anything that fails is dropped and logged, never answered: an
// unverified message cannot be trusted even to say which job to refuse.

import { verifySigned } from "./crypto.mjs";
import { JobReporter } from "./connection.mjs";
import { isRateLimited } from "./runner/claude.mjs";
import { runSetup } from "./workspace.mjs";

const MESSAGE_TYPES = new Set(["job.offer", "job.cancel", "settings.update"]);
/** After a run hits the subscription's usage limit, new offers are refused for this long. */
export const RATE_LIMIT_PAUSE_MS = 15 * 60_000;

export class Supervisor {
  constructor({ conn, deviceId, serverKey, poolSize, workspace, records, runJob, config, log = console.log, now = Date.now }) {
    this.conn = conn;
    this.deviceId = deviceId;
    this.serverKey = serverKey;
    this.poolSize = poolSize;
    this.workspace = workspace;
    this.records = records;
    this.runJob = runJob; // ({ offer, cwd, signal, onEvent, onSpawn }) => runner result
    this.config = config;
    this.log = log;
    this.now = now;
    this.jobs = new Map(); // jobId -> { offer, controller, done }
    this.pausedUntil = 0;
    this.stopping = false;
    this.onSettings = () => {};
  }

  get free() {
    return Math.max(0, this.poolSize - this.jobs.size);
  }
  running() {
    return [...this.jobs.keys()];
  }

  /** One message from the stream, `data` as sent: `{ message, signature }`. Returns what was done, for tests and the log. */
  async handle({ event, data }) {
    let parsed;
    try {
      parsed = JSON.parse(data);
    } catch {
      return this.drop("unreadable message");
    }
    const { message, signature } = parsed ?? {};
    if (!message || typeof message !== "object") return this.drop("message without a body");
    if (!verifySigned(this.serverKey, message, signature)) return this.drop(`bad signature on ${event}`);
    if (message.deviceId !== this.deviceId) return this.drop(`${event} for another device`);
    if (message.type !== event || !MESSAGE_TYPES.has(event)) return this.drop(`unknown message type ${event}`);
    if (event === "settings.update") return this.settings(message);
    if (event === "job.cancel") return this.cancel(message.jobId);
    return this.offer(message);
  }

  drop(why) {
    this.log(`dropped: ${why}`);
    return "dropped";
  }

  settings(m) {
    if (Number.isInteger(m.poolSize) && m.poolSize >= 1 && m.poolSize <= 8) this.poolSize = m.poolSize;
    this.onSettings({ poolSize: this.poolSize, chatModel: m.chatModel ?? null });
    this.log(`settings: pool size ${this.poolSize}`);
    return "settings";
  }

  cancel(jobId) {
    const job = this.jobs.get(jobId);
    if (!job) return "unknown job";
    this.log(`job ${jobId}: cancel requested`);
    job.controller.abort();
    return "canceling";
  }

  async reject(jobId, reason, detail) {
    this.log(`job ${jobId}: refused (${reason}${detail ? `: ${detail}` : ""})`);
    await this.conn.post(`/api/daemon/jobs/${jobId}/reject`, { reason, detail }).catch((e) => this.log(`job ${jobId}: could not refuse (${e.message})`));
    return "rejected";
  }

  async offer(m) {
    if (this.jobs.has(m.jobId)) return "duplicate";
    if (Date.parse(m.expiresAt) < this.now()) return this.drop(`expired offer ${m.jobId}`); // the server has already expired it
    if (!m.repository?.cloneUrl || !m.issue?.ref || !m.branch) return this.reject(m.jobId, "not_ready", "the offer is missing the repository, issue or branch");
    if (this.stopping) return this.reject(m.jobId, "not_ready", "the daemon is stopping");
    if (this.pausedUntil > this.now()) return this.reject(m.jobId, "not_ready", "paused after hitting the usage limit");
    if (this.free <= 0) return this.reject(m.jobId, "no_free_slot", `${this.jobs.size} of ${this.poolSize} running`);
    // The slot is taken before the accept goes out, so two offers arriving together cannot both get the last one.
    const controller = new AbortController();
    const job = { offer: m, controller, done: null };
    this.jobs.set(m.jobId, job);
    try {
      await this.conn.post(`/api/daemon/jobs/${m.jobId}/accept`);
    } catch (e) {
      this.jobs.delete(m.jobId);
      this.log(`job ${m.jobId}: could not accept (${e.message})`);
      return "accept failed";
    }
    this.log(`job ${m.jobId}: accepted ${m.issue.ref} on ${m.repository.fullName ?? m.repository.name} with ${m.model}`);
    job.done = this.execute(m, controller.signal).finally(() => this.jobs.delete(m.jobId));
    return "accepted";
  }

  /** The job itself: worktree, setup, agent, commit, push, report. Never throws: every failure becomes the job's finish. */
  async execute(m, signal) {
    const reporter = new JobReporter(this.conn, m.jobId, { log: this.log });
    const started = this.now();
    const phase = (name, detail) => reporter.event("phase", detail ? { phase: name, detail } : { phase: name });
    this.records.write({ id: m.jobId, repositoryId: m.repository.id, branch: m.branch, startedAt: new Date(started).toISOString(), pid: null });
    let cwd = null;
    let outcome;
    try {
      await this.workspace.ensureClone(m.repository, phase);
      if (signal.aborted) throw new Canceled();
      // A continuation starts from the job's own branch as the earlier run left it; a fresh run from its base.
      const startFrom = m.startFrom ?? m.baseBranch;
      phase("worktree", m.previous ? `${m.branch}, continuing from its last push` : `${m.branch} from ${startFrom ?? m.repository.defaultBranch}`);
      const wt = await this.workspace.createWorktree(m.repository, m.jobId, m.branch, startFrom);
      cwd = wt.dir;
      const setup = this.config.repositories?.[m.repository.id]?.setup;
      if (setup) {
        phase("setup", setup);
        await runSetup(cwd, setup, signal);
      }
      if (signal.aborted) throw new Canceled();
      phase("agent", m.model);
      const r = await this.runJob({
        offer: m,
        cwd,
        signal,
        onEvent: (e) => reporter.event(e.type, e.data),
        onSpawn: (pid) => this.records.update(m.jobId, { pid }),
      });
      if (r.result && isRateLimited(r.result.text)) {
        this.pausedUntil = this.now() + RATE_LIMIT_PAUSE_MS;
        this.log(`usage limit reached: refusing new jobs for ${RATE_LIMIT_PAUSE_MS / 60_000} minutes`);
      }
      // Whatever happened, keep the agent's work: commit what it left and push the branch if there is anything on it.
      phase("pushing", m.branch);
      await this.workspace.commitLeftovers(cwd, `${m.issue.ref}: work in progress from the agent run`);
      const sum = await this.workspace.summary(cwd, wt.base);
      let pushed = false;
      if (sum.ahead > 0) {
        await this.workspace.push(cwd, m.branch);
        pushed = true;
      }
      const result = { commit: sum.commit, pushed, diffStats: sum.diffStats, costUsd: r.result?.costUsd ?? null, durationMs: this.now() - started };
      if (r.stopped === "canceled" || signal.aborted) outcome = { state: "canceled", ...result, error: this.stopReason() };
      else if (r.stopped === "timeout") outcome = { state: "failed", ...result, error: `The agent ran past the ${this.config.runner.timeoutMinutes}-minute limit` };
      else if (!r.ok) outcome = { state: "failed", ...result, error: r.error ?? r.result?.text ?? (r.stderr || `claude exited with ${r.exitCode}`) };
      else outcome = { state: "succeeded", ...result };
    } catch (e) {
      outcome = e instanceof Canceled || signal.aborted ? { state: "canceled", error: this.stopReason() } : { state: "failed", error: e.message };
      reporter.event("error", { message: String(e.message).slice(0, 1000) });
    }
    await reporter.finish({ ...outcome, durationMs: this.now() - started });
    this.records.remove(m.jobId);
    if (cwd && outcome.state === "succeeded" && outcome.pushed) await this.workspace.removeWorktree(m.repository.id, m.jobId).catch(() => {});
    this.log(`job ${m.jobId}: ${outcome.state}${outcome.error ? ` (${outcome.error})` : ""}`);
    return outcome;
  }

  /** Why a run was stopped: the daemon going down is not the same as someone pressing Stop. */
  stopReason() {
    return this.stopping ? "The daemon was stopped while this ran. Run it again once it is back." : "Stopped on request";
  }

  /** Stop taking work and cancel what is running; resolves when every job has reported. */
  async shutdown() {
    this.stopping = true;
    for (const j of this.jobs.values()) j.controller.abort();
    await Promise.all([...this.jobs.values()].map((j) => j.done));
  }
}

class Canceled extends Error {
  constructor() {
    super("Stopped on request");
  }
}
