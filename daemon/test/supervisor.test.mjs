import { test, beforeEach } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import { canonicalJson, publicKeyFromRaw, rawPublicKey } from "../src/crypto.mjs";
import { Supervisor } from "../src/supervisor.mjs";

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
  const workspace = {
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

test("shutdown cancels what is running and waits for it to report", async () => {
  const m = offer();
  await s.sup.handle(sign(m));
  await s.sup.shutdown();
  assert.equal(s.posts.find((p) => p.path.endsWith("/finish")).body.state, "canceled");
  assert.equal(await s.sup.handle(sign(offer())), "rejected");
});
