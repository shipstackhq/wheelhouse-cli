// The pool: which jobs this machine is running, whether it can take another, and each job from offer to finish.
//
// Every message from Wheelhouse is checked before anything happens: the server's signature against the key pinned at pairing, that it
// names this device, and (for an offer) that it has not expired. Anything that fails is dropped and logged, never answered: an
// unverified message cannot be trusted even to say which job to refuse.

import { verifySigned } from "./crypto.mjs";
import { JobReporter } from "./connection.mjs";
import { isRateLimited } from "./runner/claude.mjs";
import { runSetup } from "./workspace.mjs";

const MESSAGE_TYPES = new Set(["job.offer", "job.cancel", "settings.update", "chat.message", "chat.cancel"]);
/** Why a run whose branch a newer run took over was stopped: its work is kept on this machine, not pushed. */
export const SUPERSEDED_REASON = "Stopped: a newer run took over this branch while this machine could not be reached. Nothing was pushed; the worktree is kept here.";
/** Chat answers run in their own small pool, apart from the jobs: they never take a job's slot and do not show in `free`. */
export const CHAT_POOL_SIZE = 2;
/** After a run hits the subscription's usage limit, new offers are refused for this long. */
export const RATE_LIMIT_PAUSE_MS = 15 * 60_000;

export class Supervisor {
  constructor({ conn, deviceId, serverKey, poolSize, workspace, records, runJob, runChat, config, log = console.log, now = Date.now, finishRetryMs }) {
    this.conn = conn;
    this.deviceId = deviceId;
    this.serverKey = serverKey;
    this.poolSize = poolSize;
    this.workspace = workspace;
    this.records = records;
    this.runJob = runJob; // ({ offer, cwd, signal, onEvent, onSpawn }) => runner result
    this.runChat = runChat; // ({ message, commit, cwd, signal, onEvent }) => runner result
    this.config = config;
    this.log = log;
    this.now = now;
    this.finishRetryMs = finishRetryMs;
    this.jobs = new Map(); // jobId -> { offer, controller, done }
    this.chats = new Map(); // messageId -> { message, controller, done }
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
  /** Jobs that ended here but whose finish Wheelhouse has not received yet: still ours, so they are listed with the running ones. */
  pending() {
    return (this.records.all?.() ?? []).filter((r) => r.pendingFinish).map((r) => r.id);
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
    if (event === "job.cancel") return this.cancel(message.jobId, { superseded: message.superseded === true });
    if (event === "chat.cancel") return this.cancelChat(message.messageId);
    if (event === "chat.message") return this.chat(message);
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

  /** `superseded`: a newer run took this one's branch over, so it stops without committing or pushing anything. */
  cancel(jobId, { superseded = false } = {}) {
    const job = this.jobs.get(jobId);
    if (!job) return "unknown job";
    this.log(`job ${jobId}: ${superseded ? "stop requested, a newer run took over its branch" : "cancel requested"}`);
    if (superseded) job.superseded = true;
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

  cancelChat(messageId) {
    const chat = this.chats.get(messageId);
    if (!chat) return "unknown chat";
    this.log(`chat ${messageId}: cancel requested`);
    chat.controller.abort();
    return "canceling";
  }

  /** A chat message that cannot be answered here is finished as failed, so the person sees why instead of waiting. */
  async failChat(messageId, error) {
    this.log(`chat ${messageId}: refused (${error})`);
    await this.conn.post(`/api/daemon/chat/${messageId}/finish`, { state: "failed", error }).catch((e) => this.log(`chat ${messageId}: could not report (${e.message})`));
    return "rejected";
  }

  async chat(m) {
    if (typeof m.messageId !== "string") return this.drop("chat message without an id");
    if (this.chats.has(m.messageId)) return "duplicate";
    if (Date.parse(m.expiresAt) < this.now()) return this.drop(`expired chat message ${m.messageId}`);
    if (!m.threadId || !m.model || typeof m.prompt !== "string" || !m.prompt || !m.repository?.id || !m.repository.cloneUrl) return this.failChat(m.messageId, "The message to this device was incomplete");
    if (this.stopping) return this.failChat(m.messageId, "The daemon is stopping");
    if (this.pausedUntil > this.now()) return this.failChat(m.messageId, "This machine is paused after hitting the Claude usage limit; try again later");
    if (this.chats.size >= CHAT_POOL_SIZE) return this.failChat(m.messageId, `This device is already answering ${this.chats.size} chats; try again in a moment`);
    const controller = new AbortController();
    const chat = { message: m, controller, done: null };
    this.chats.set(m.messageId, chat);
    chat.done = this.executeChat(m, controller.signal).finally(() => this.chats.delete(m.messageId));
    return "accepted";
  }

  /** One chat answer: the thread's worktree, the agent, the report. Never throws: every failure becomes the message's finish. */
  async executeChat(m, signal) {
    const reporter = new JobReporter(this.conn, m.messageId, { path: `/api/daemon/chat/${m.messageId}`, log: this.log, ...(this.finishRetryMs === undefined ? {} : { finishRetryMs: this.finishRetryMs }) });
    let outcome;
    try {
      await this.workspace.ensureClone(m.repository);
      if (signal.aborted) throw new Canceled();
      const wt = await this.workspace.chatWorktree(m.repository, m.threadId);
      const r = await this.runChat({
        message: m,
        checkout: wt,
        cwd: wt.dir,
        signal,
        onEvent: (e) => (e.type === "delta" || e.type === "tool") && reporter.event(e.type, e.data),
      });
      // An answer may talk about rate limits; only a run that failed on one pauses the machine.
      if (r.result?.isError && isRateLimited(r.result.text)) {
        this.pausedUntil = this.now() + RATE_LIMIT_PAUSE_MS;
        this.log(`usage limit reached: refusing new work for ${RATE_LIMIT_PAUSE_MS / 60_000} minutes`);
      }
      const result = { sessionId: r.result?.sessionId ?? undefined, costUsd: r.result?.costUsd ?? undefined };
      if (r.stopped === "canceled" || signal.aborted) outcome = { state: "canceled", ...result, error: this.stopReason() };
      else if (r.stopped === "timeout") outcome = { state: "failed", ...result, error: "The answer took too long and was stopped" };
      else if (!r.ok) outcome = { state: "failed", ...result, error: r.error ?? r.result?.text ?? (r.stderr || `claude exited with ${r.exitCode}`) };
      else outcome = { state: "done", ...result, text: r.result.text };
    } catch (e) {
      outcome = e instanceof Canceled || signal.aborted ? { state: "canceled", error: this.stopReason() } : { state: "failed", error: e.message };
    }
    await reporter.finish(outcome);
    this.log(`chat ${m.messageId}: ${outcome.state}${outcome.error ? ` (${outcome.error})` : ""}`);
    return outcome;
  }

  /** The job itself: worktree, setup, agent, commit, push, report. Never throws: every failure becomes the job's finish. */
  async execute(m, signal) {
    const reporter = new JobReporter(this.conn, m.jobId, { log: this.log, ...(this.finishRetryMs === undefined ? {} : { finishRetryMs: this.finishRetryMs }) });
    const started = this.now();
    const phase = (name, detail) => reporter.event("phase", detail ? { phase: name, detail } : { phase: name });
    this.records.write({ id: m.jobId, repositoryId: m.repository.id, branch: m.branch, startedAt: new Date(started).toISOString(), pid: null });
    let cwd = null;
    let outcome;
    const review = m.mode === "review" || m.push === false;
    const superseded = () => this.jobs.get(m.jobId)?.superseded === true;
    const stopped = () => (superseded() ? SUPERSEDED_REASON : this.stopReason());
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
      // Whatever happened, keep the agent's work: commit what it left and push the branch if there is anything on it. Not when a newer
      // run took the branch over: pushing could overwrite its commits, so the work stays in this worktree for a person to look at.
      if (superseded()) {
        outcome = { state: "canceled", pushed: false, costUsd: r.result?.costUsd ?? null, error: SUPERSEDED_REASON };
        throw new Superseded();
      }
      // A review (WH-122) only reads: nothing is committed or pushed, and what it says is the result.
      let sum = { commit: null, diffStats: {}, ahead: 0 };
      let pushed = false;
      if (!review) {
        phase("pushing", m.branch);
        await this.workspace.commitLeftovers(cwd, `${m.issue.ref}: work in progress from the agent run`);
        sum = await this.workspace.summary(cwd, wt.base);
        if (sum.ahead > 0) {
          await this.workspace.push(cwd, m.branch);
          pushed = true;
        }
      }
      const result = { commit: sum.commit, pushed, diffStats: sum.diffStats, costUsd: r.result?.costUsd ?? null, durationMs: this.now() - started, ...(r.result?.learnings?.length ? { learnings: r.result.learnings } : {}) };
      if (r.stopped === "canceled" || signal.aborted) outcome = { state: "canceled", ...result, error: stopped() };
      else if (r.stopped === "timeout") outcome = { state: "failed", ...result, error: `The agent ran past the ${this.config.runner.timeoutMinutes}-minute limit` };
      else if (!r.ok) outcome = { state: "failed", ...result, error: r.error ?? r.result?.text ?? (r.stderr || `claude exited with ${r.exitCode}`) };
      else outcome = { state: "succeeded", ...result };
    } catch (e) {
      if (!(e instanceof Superseded)) {
        outcome = e instanceof Canceled || signal.aborted ? { state: "canceled", error: stopped() } : { state: "failed", error: e.message };
        reporter.event("error", { message: String(e.message).slice(0, 1000) });
      }
    }
    const report = { ...outcome, durationMs: this.now() - started };
    const { retry } = await reporter.finish(report);
    // A finish that could not get through (no connection) is kept with the job's record and sent again once Wheelhouse is reachable.
    if (retry) this.records.update(m.jobId, { pendingFinish: report });
    else this.records.remove(m.jobId);
    if (cwd && outcome.state === "succeeded" && (outcome.pushed || review)) await this.workspace.removeWorktree(m.repository.id, m.jobId).catch(() => {});
    this.log(`job ${m.jobId}: ${outcome.state}${outcome.error ? ` (${outcome.error})` : ""}`);
    return outcome;
  }

  /** Why a run was stopped: the daemon going down is not the same as someone pressing Stop. */
  stopReason() {
    return this.stopping ? "The daemon was stopped while this ran. Run it again once it is back." : "Stopped on request";
  }

  /** Stop taking work and cancel what is running; resolves when every job and chat has reported. */
  async shutdown() {
    this.stopping = true;
    const all = [...this.jobs.values(), ...this.chats.values()];
    for (const j of all) j.controller.abort();
    await Promise.all(all.map((j) => j.done));
  }
}

class Superseded extends Error {}

class Canceled extends Error {
  constructor() {
    super("Stopped on request");
  }
}
