import { test, beforeEach } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import { canonicalJson, publicKeyFromRaw, rawPublicKey } from "../src/crypto.mjs";
import { Supervisor, SUPERSEDED_REASON } from "../src/supervisor.mjs";

/** The server's key, and messages signed with it the way Wheelhouse signs them. */
const server = crypto.generateKeyPairSync("ed25519");
const DEVICE = "11111111-1111-1111-1111-111111111111";
const REPO = "22222222-2222-2222-2222-222222222222";
const sign = (message, key = server.privateKey) => ({ event: message.type, data: JSON.stringify({ message, signature: crypto.sign(null, Buffer.from(canonicalJson(message)), key).toString("base64url") }) });

let n = 0;
const offer = (o = {}) => ({
  type: "job.offer",
  deviceId: DEVICE,
  issuedAt: new Date().toISOString(),
  jobId: `00000000-0000-0000-0000-${String(++n).padStart(12, "0")}`,
  expiresAt: new Date(Date.now() + 60_000).toISOString(),
  model: "sonnet",
  branch: `wh/WH-${n}-x`,
  boardAccess: "none",
  issue: { id: "i", ref: `WH-${n}`, title: "Do it", body: "" },
  repository: { id: REPO, name: "app", fullName: "acme/app", cloneUrl: "https://github.com/acme/app.git", defaultBranch: "main", pathLimit: null },
  ...o,
});

let c = 0;
const chatMessage = (o = {}) => ({
  type: "chat.message",
  deviceId: DEVICE,
  issuedAt: new Date().toISOString(),
  messageId: `99999999-0000-0000-0000-${String(++c).padStart(12, "0")}`,
  threadId: "88888888-0000-0000-0000-000000000001",
  model: "sonnet",
  sessionId: null,
  prompt: "Where is auth handled?",
  expiresAt: new Date(Date.now() + 60_000).toISOString(),
  repository: { id: REPO, name: "app", fullName: "acme/app", cloneUrl: "https://github.com/acme/app.git", defaultBranch: "main" },
  ...o,
});

/** A fake line to Wheelhouse that records every POST; a fake workspace; a runner that waits until released or aborted. */
function setup({ poolSize = 2 } = {}) {
  const posts = [];
  const conn = { post: async (path, body) => (posts.push({ path, body }), {}) };
  const releases = new Map();
  const runJob = ({ offer: m, signal, onEvent }) =>
    new Promise((resolve) => {
      onEvent({ type: "started", data: {} });
      releases.set(m.jobId, () => resolve({ ok: true, exitCode: 0, result: { costUsd: 0.1, text: "done" } }));
      signal.addEventListener("abort", () => resolve({ ok: false, stopped: "canceled", exitCode: null, result: null }));
    });
  const runChat = ({ message: m, signal, onEvent }) =>
    new Promise((resolve) => {
      onEvent({ type: "started", data: {} });
      onEvent({ type: "delta", data: { text: "In auth/" } });
      onEvent({ type: "tool", data: { name: "Read", summary: "auth/index.ts" } });
      releases.set(m.messageId, () => resolve({ ok: true, exitCode: 0, result: { isError: false, sessionId: "s1", costUsd: 0.02, text: "In auth/index.ts" } }));
      signal.addEventListener("abort", () => resolve({ ok: false, stopped: "canceled", exitCode: null, result: null }));
    });
  const workspace = {
    chatWorktree: async (_r, threadId) => ({ dir: `/chat/${threadId}`, commit: "abc1234", branches: ["origin/main"] }),
    ensureClone: async () => "/repo",
    createWorktree: async (_r, jobId) => ({ dir: `/wt/${jobId}`, base: "origin/main" }),
    commitLeftovers: async () => false,
    summary: async () => ({ ahead: 1, commit: "abc1234", diffStats: { files: 1, insertions: 2, deletions: 0 } }),
    push: async () => {},
    removeWorktree: async () => {},
  };
  const records = { write() {}, update() {}, remove() {} };
  const logs = [];
  const sup = new Supervisor({
    conn,
    deviceId: DEVICE,
    serverKey: publicKeyFromRaw(rawPublicKey(server.publicKey)),
    poolSize,
    workspace,
    records,
    runJob,
    runChat,
    config: { runner: { timeoutMinutes: 45 }, repositories: {} },
    log: (m) => logs.push(m),
  });
  return { sup, posts, releases, logs };
}

let s;
beforeEach(() => (s = setup()));

test("a verified offer is accepted, run, and finished with what the run reported", async () => {
  const m = offer();
  assert.equal(await s.sup.handle(sign(m)), "accepted");
  assert.deepEqual(s.sup.running(), [m.jobId]);
  s.releases.get(m.jobId)();
  await s.sup.jobs.get(m.jobId)?.done;
  const paths = s.posts.map((p) => p.path);
  assert.equal(paths[0], `/api/daemon/jobs/${m.jobId}/accept`);
  const finish = s.posts.find((p) => p.path.endsWith("/finish"));
  assert.equal(finish.body.state, "succeeded");
  assert.equal(finish.body.commit, "abc1234");
  assert.equal(finish.body.pushed, true);
  assert.ok(s.posts.some((p) => p.path.endsWith("/events") && p.body.events.some((e) => e.type === "started")));
  assert.deepEqual(s.sup.running(), []);
});

test("unverifiable messages are dropped without a word to the server", async () => {
  const forged = sign(offer(), crypto.generateKeyPairSync("ed25519").privateKey);
  assert.equal(await s.sup.handle(forged), "dropped");
  assert.equal(await s.sup.handle(sign(offer({ deviceId: "someone-else" }))), "dropped");
  const m = offer();
  const tampered = JSON.parse(sign(m).data);
  tampered.message.repository.cloneUrl = "https://evil.example/x.git";
  assert.equal(await s.sup.handle({ event: "job.offer", data: JSON.stringify(tampered) }), "dropped");
  assert.equal(await s.sup.handle({ event: "job.cancel", data: sign(m).data }), "dropped", "the event must match the signed type");
  assert.equal(await s.sup.handle(sign(offer({ expiresAt: new Date(Date.now() - 1000).toISOString() }))), "dropped");
  assert.equal(await s.sup.handle({ event: "job.offer", data: "not json" }), "dropped");
  assert.deepEqual(s.posts, []);
});

test("never more than the pool size at once; the next offer is refused for want of a slot", async () => {
  const a = offer();
  const b = offer();
  const c = offer();
  await s.sup.handle(sign(a));
  await s.sup.handle(sign(b));
  assert.equal(await s.sup.handle(sign(c)), "rejected");
  const r = s.posts.find((p) => p.path === `/api/daemon/jobs/${c.jobId}/reject`);
  assert.equal(r.body.reason, "no_free_slot");
  assert.equal(await s.sup.handle(sign(a)), "duplicate");
  s.releases.get(a.jobId)();
  await s.sup.jobs.get(a.jobId)?.done;
  assert.equal(s.sup.free, 1);
});

test("a settings message changes the pool size", async () => {
  const seen = [];
  s.sup.onSettings = (x) => seen.push(x);
  assert.equal(await s.sup.handle(sign({ type: "settings.update", deviceId: DEVICE, issuedAt: "x", poolSize: 1, chatModel: null })), "settings");
  assert.equal(s.sup.poolSize, 1);
  assert.deepEqual(seen, [{ poolSize: 1, chatModel: null }]);
  await s.sup.handle(sign(offer()));
  assert.equal(await s.sup.handle(sign(offer())), "rejected");
});

test("a signed cancel stops the run; it is reported canceled, and the work still pushed", async () => {
  const m = offer();
  await s.sup.handle(sign(m));
  const done = s.sup.jobs.get(m.jobId).done;
  assert.equal(await s.sup.handle(sign({ type: "job.cancel", deviceId: DEVICE, issuedAt: "x", jobId: m.jobId })), "canceling");
  const outcome = await done;
  assert.equal(outcome.state, "canceled");
  assert.equal(outcome.pushed, true);
  assert.equal(s.posts.find((p) => p.path.endsWith("/finish")).body.state, "canceled");
});

test("a failing step fails the job with the reason, and frees the slot", async () => {
  s.sup.workspace.createWorktree = async () => {
    throw new Error("fatal: invalid reference: origin/main");
  };
  const m = offer();
  await s.sup.handle(sign(m));
  const outcome = await s.sup.jobs.get(m.jobId)?.done;
  assert.equal(outcome.state, "failed");
  assert.match(outcome.error, /invalid reference/);
  assert.equal(s.sup.free, 2);
});

test("a run that hits the usage limit pauses new offers", async () => {
  s.sup.runJob = async () => ({ ok: false, exitCode: 1, result: { isError: true, text: "Claude usage limit reached. Your limit will reset at 5pm" } });
  const m = offer();
  await s.sup.handle(sign(m));
  await s.sup.jobs.get(m.jobId)?.done;
  const next = offer();
  assert.equal(await s.sup.handle(sign(next)), "rejected");
  assert.match(s.posts.find((p) => p.path === `/api/daemon/jobs/${next.jobId}/reject`).body.detail, /usage limit/);
});

test("a Stop from Wheelhouse says it was requested; the daemon going down says that instead", async () => {
  const a = offer();
  await s.sup.handle(sign(a));
  await s.sup.handle(sign({ type: "job.cancel", deviceId: DEVICE, issuedAt: "x", jobId: a.jobId }));
  assert.equal((await s.sup.jobs.get(a.jobId)?.done)?.error, "Stopped on request");
  const b = offer();
  await s.sup.handle(sign(b));
  await s.sup.shutdown();
  const finish = s.posts.find((p) => p.path === `/api/daemon/jobs/${b.jobId}/finish`);
  assert.equal(finish.body.state, "canceled");
  assert.match(finish.body.error, /daemon was stopped/);
});

test("shutdown cancels what is running and waits for it to report", async () => {
  const m = offer();
  await s.sup.handle(sign(m));
  await s.sup.shutdown();
  assert.equal(s.posts.find((p) => p.path.endsWith("/finish")).body.state, "canceled");
  assert.equal(await s.sup.handle(sign(offer())), "rejected");
});

test("a run whose branch a newer run took over stops without committing or pushing, and keeps its worktree", async () => {
  const calls = [];
  s.sup.workspace.commitLeftovers = async () => (calls.push("commit"), false);
  s.sup.workspace.push = async () => calls.push("push");
  s.sup.workspace.removeWorktree = async () => calls.push("remove");
  const m = offer();
  await s.sup.handle(sign(m));
  const done = s.sup.jobs.get(m.jobId).done;
  assert.equal(await s.sup.handle(sign({ type: "job.cancel", deviceId: DEVICE, issuedAt: "x", jobId: m.jobId, superseded: true })), "canceling");
  const outcome = await done;
  assert.deepEqual(calls, [], "nothing committed, pushed or removed");
  assert.equal(outcome.state, "canceled");
  assert.equal(outcome.pushed, false);
  assert.equal(outcome.error, SUPERSEDED_REASON);
  assert.equal(s.posts.find((p) => p.path.endsWith("/finish")).body.error, SUPERSEDED_REASON);
});

test("a finish that cannot get through is kept with the job and listed with it until it is sent", async () => {
  const stored = new Map();
  s.sup.records = {
    write: (r) => stored.set(r.id, r),
    update: (id, patch) => stored.has(id) && stored.set(id, { ...stored.get(id), ...patch }),
    remove: (id) => stored.delete(id),
    all: () => [...stored.values()],
  };
  s.sup.finishRetryMs = 1;
  s.sup.conn.post = async (path, body) => {
    s.posts.push({ path, body });
    if (path.endsWith("/finish")) throw new Error("fetch failed");
    return {};
  };
  const m = offer();
  await s.sup.handle(sign(m));
  const done = s.sup.jobs.get(m.jobId).done;
  s.releases.get(m.jobId)();
  await done;
  assert.equal(stored.get(m.jobId).pendingFinish.state, "succeeded");
  assert.deepEqual(s.sup.pending(), [m.jobId]);
  assert.deepEqual(s.sup.running(), []);
});

test("a finish Wheelhouse refuses for good is not kept", async () => {
  const stored = new Map();
  s.sup.records = { write: (r) => stored.set(r.id, r), update: () => {}, remove: (id) => stored.delete(id), all: () => [...stored.values()] };
  s.sup.finishRetryMs = 1;
  const { HttpError } = await import("../src/connection.mjs");
  s.sup.conn.post = async (path, body) => {
    s.posts.push({ path, body });
    if (path.endsWith("/finish")) throw new HttpError(409, "The job is canceled");
    return {};
  };
  const m = offer();
  await s.sup.handle(sign(m));
  const done = s.sup.jobs.get(m.jobId).done;
  s.releases.get(m.jobId)();
  await done;
  assert.deepEqual(s.sup.pending(), []);
  assert.equal(stored.has(m.jobId), false);
});

const chatFinish = (m) => s.posts.find((p) => p.path === `/api/daemon/chat/${m.messageId}/finish`);

test("a verified chat message is run in its own pool, streamed, and finished with the answer and session", async () => {
  const m = chatMessage();
  assert.equal(await s.sup.handle(sign(m)), "accepted");
  assert.deepEqual(s.sup.running(), [], "not a job");
  assert.equal(s.sup.free, 2, "takes no job slot");
  s.releases.get(m.messageId)();
  await s.sup.chats.get(m.messageId)?.done;
  const events = s.posts.find((p) => p.path === `/api/daemon/chat/${m.messageId}/events`).body.events;
  assert.deepEqual(events.map((e) => [e.seq, e.type]), [[0, "delta"], [1, "tool"]], "only delta and tool go up");
  assert.deepEqual(chatFinish(m).body, { state: "done", sessionId: "s1", costUsd: 0.02, text: "In auth/index.ts" });
  assert.equal(s.sup.chats.size, 0);
});

test("chat messages are verified like offers, and an expired one is dropped", async () => {
  assert.equal(await s.sup.handle(sign(chatMessage(), crypto.generateKeyPairSync("ed25519").privateKey)), "dropped");
  assert.equal(await s.sup.handle(sign(chatMessage({ deviceId: "someone-else" }))), "dropped");
  assert.equal(await s.sup.handle(sign(chatMessage({ expiresAt: new Date(Date.now() - 1000).toISOString() }))), "dropped");
  assert.equal(await s.sup.handle({ event: "chat.cancel", data: sign(chatMessage()).data }), "dropped", "the event must match the signed type");
  assert.deepEqual(s.posts, []);
});

test("the chat pool is two, apart from jobs: a third message fails cleanly, and jobs still run", async () => {
  const [a, b, third] = [chatMessage(), chatMessage(), chatMessage()];
  await s.sup.handle(sign(a));
  await s.sup.handle(sign(b));
  assert.equal(await s.sup.handle(sign(third)), "rejected");
  assert.equal(chatFinish(third).body.state, "failed");
  assert.match(chatFinish(third).body.error, /already answering 2/);
  assert.equal(await s.sup.handle(sign(offer())), "accepted", "chats do not use the job pool");
  assert.equal(await s.sup.handle(sign(a)), "duplicate");
});

test("a signed chat.cancel stops the run, reported canceled", async () => {
  const m = chatMessage();
  await s.sup.handle(sign(m));
  const done = s.sup.chats.get(m.messageId).done;
  assert.equal(await s.sup.handle(sign({ type: "chat.cancel", deviceId: DEVICE, issuedAt: "x", messageId: m.messageId })), "canceling");
  assert.equal((await done).state, "canceled");
  assert.equal(chatFinish(m).body.state, "canceled");
  assert.equal(await s.sup.handle(sign({ type: "chat.cancel", deviceId: DEVICE, issuedAt: "x", messageId: m.messageId })), "unknown chat");
});

test("a malformed chat message fails with a reason; one with no id is dropped", async () => {
  const m = chatMessage({ prompt: "" });
  assert.equal(await s.sup.handle(sign(m)), "rejected");
  assert.match(chatFinish(m).body.error, /incomplete/);
  assert.equal(await s.sup.handle(sign(chatMessage({ messageId: undefined }))), "dropped");
});

test("a chat run that fails on the usage limit pauses jobs and chats; an answer that merely mentions it does not", async () => {
  s.sup.runChat = async () => ({ ok: true, exitCode: 0, result: { isError: false, text: "Set a rate limit of 100/min", sessionId: "s", costUsd: null } });
  const a = chatMessage();
  await s.sup.handle(sign(a));
  await s.sup.chats.get(a.messageId)?.done;
  assert.equal(chatFinish(a).body.state, "done");
  assert.equal(await s.sup.handle(sign(offer())), "accepted");

  s.sup.runChat = async () => ({ ok: false, exitCode: 1, result: { isError: true, text: "Claude usage limit reached", sessionId: null, costUsd: null } });
  const b = chatMessage();
  await s.sup.handle(sign(b));
  await s.sup.chats.get(b.messageId)?.done;
  assert.equal(chatFinish(b).body.state, "failed");
  const next = chatMessage();
  assert.equal(await s.sup.handle(sign(next)), "rejected");
  assert.match(chatFinish(next).body.error, /paused/);
  assert.equal(await s.sup.handle(sign(offer())), "rejected");
});

test("a failing worktree fails the message with the reason", async () => {
  s.sup.workspace.chatWorktree = async () => {
    throw new Error("the branch main does not exist");
  };
  const m = chatMessage();
  await s.sup.handle(sign(m));
  await s.sup.chats.get(m.messageId)?.done;
  assert.deepEqual(chatFinish(m).body, { state: "failed", error: "the branch main does not exist" });
});

test("shutdown cancels running chats and waits for them to report", async () => {
  const m = chatMessage();
  await s.sup.handle(sign(m));
  await s.sup.shutdown();
  assert.equal(chatFinish(m).body.state, "canceled");
});
