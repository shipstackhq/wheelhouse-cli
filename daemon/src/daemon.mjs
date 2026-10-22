// The running daemon: connect, say hello, keep a heartbeat, read the stream and hand its messages to the supervisor, reconnect with
// backoff when the line drops. It stops for good when Wheelhouse no longer accepts its key (the device was unpaired).

import { capabilities } from "./capabilities.mjs";
import { loadConfig, paths, updateConfig } from "./config.mjs";
import { Connection, HttpError, UnpairedError } from "./connection.mjs";
import { importPrivateKey, publicKeyFromRaw } from "./crypto.mjs";
import { adoptUserPath, findClaude } from "./environment.mjs";
import { loadKey } from "./keystore.mjs";
import { runClaude } from "./runner/claude.mjs";
import { JobRecords } from "./state.mjs";
import { Supervisor } from "./supervisor.mjs";
import { Workspace } from "./workspace.mjs";

export const VERSION = "0.1.0";
export const HEARTBEAT_MS = 30_000;

const stamp = () => new Date().toISOString();
export const log = (msg) => console.log(`${stamp()} ${msg}`);

/** Wait 1s, 2s, 4s… up to a minute, with some jitter so many machines do not reconnect in step. */
export const backoffMs = (failures) => Math.min(60_000, 1000 * 2 ** Math.min(failures, 6)) * (0.75 + Math.random() * 0.5);

const sleep = (ms, signal) =>
  new Promise((resolve) => {
    const t = setTimeout(resolve, ms);
    signal?.addEventListener("abort", () => (clearTimeout(t), resolve()), { once: true });
  });

/** A job a previous run of the daemon was doing when it stopped: stop the agent if it is still alive, and report the job failed. */
async function recover({ conn, records, workspace }) {
  for (const r of records.all()) {
    if (r.pid) {
      try {
        process.kill(-r.pid, "SIGTERM");
        log(`job ${r.id}: stopped the agent a previous run left behind (pid ${r.pid})`);
      } catch {
        /* not running any more */
      }
    }
    await conn.post(`/api/daemon/jobs/${r.id}/finish`, { state: "failed", error: "The daemon restarted while the job ran" }).catch((e) => log(`job ${r.id}: could not report (${e.message})`));
    records.remove(r.id);
    log(`job ${r.id}: reported as failed after a restart (its worktree is kept for a look)`);
  }
  await workspace.pruneOld(loadConfig().keepFailedHours).catch(() => {});
}

export async function runDaemon({ signal } = {}) {
  const p = paths();
  const config = loadConfig(p);
  if (!config.deviceId || !config.server || !config.serverPublicKey) throw new Error("This machine is not paired. Run: wheelhouse-daemon pair --server <url>");
  const secret = loadKey(config.keystore, p.home);
  if (!secret) throw new Error(`The device key is missing from the ${config.keystore}. Pair again: wheelhouse-daemon pair --server ${config.server}`);

  // Under launchd the PATH is bare: take the user's own, then find claude on it.
  adoptUserPath();
  const claudePath = findClaude(config.claudePath) ?? config.claudePath;

  const conn = new Connection({ server: config.server, deviceId: config.deviceId, privateKey: importPrivateKey(secret) });
  const workspace = new Workspace(p, { protocol: config.gitProtocol, log });
  const records = new JobRecords(p.jobs);
  const supervisor = new Supervisor({
    conn,
    deviceId: config.deviceId,
    serverKey: publicKeyFromRaw(config.serverPublicKey),
    poolSize: config.poolSize,
    workspace,
    records,
    config,
    log,
    runJob: ({ offer, cwd, signal: s, onEvent, onSpawn }) => runClaude({ offer, cwd, runner: config.runner, claudePath, signal: s, onEvent, onSpawn }),
  });
  supervisor.onSettings = (s) => updateConfig({ poolSize: s.poolSize, chatModel: s.chatModel }, p);

  log(`wheelhouse-daemon ${VERSION}: ${config.name ?? "this machine"} → ${config.server} (${config.workspace?.name ?? "workspace"})`);
  log(`claude: ${claudePath}`);
  const stop = new AbortController();
  signal?.addEventListener("abort", () => stop.abort(), { once: true });

  const hello = async () => {
    const caps = await capabilities({ claudePath, heldRepositories: workspace.held() });
    if (!caps.claude.installed) log(`claude was not found (looked for "${config.claudePath}" on ${process.env.PATH}): jobs will fail until it is installed, or set "claudePath" in config.json`);
    else if (!caps.claude.signedIn) log("claude is installed but not signed in: run `claude` once and sign in");
    const r = await conn.post("/api/daemon/hello", { version: VERSION, capabilities: caps, jobs: supervisor.running().map((id) => ({ id, state: "running" })) });
    if (r?.device?.poolSize && r.device.poolSize !== supervisor.poolSize) supervisor.settings({ type: "settings.update", poolSize: r.device.poolSize, chatModel: r.device.chatModel });
  };
  // Only a change is logged (it stopped working, it works again), so the log shows when Wheelhouse could not hear this machine.
  let heartbeatFailing = false;
  const heartbeat = setInterval(async () => {
    try {
      await conn.post("/api/daemon/heartbeat", { running: supervisor.running(), free: supervisor.free, onBattery: false });
      if (heartbeatFailing) log("heartbeat: reaching Wheelhouse again");
      heartbeatFailing = false;
    } catch (e) {
      if (e instanceof UnpairedError) stop.abort();
      if (!heartbeatFailing) log(`heartbeat: cannot reach Wheelhouse (${e instanceof HttpError ? `${e.status} ${e.message}` : e.message}); it shows this machine offline until this works`);
      heartbeatFailing = true;
    }
  }, HEARTBEAT_MS);
  heartbeat.unref();

  let unpaired = false;
  try {
    await recover({ conn, records, workspace });
    let failures = 0;
    let lastEventId = config.lastEventId;
    while (!stop.signal.aborted) {
      try {
        await hello();
        log("connected");
        for await (const m of conn.stream(lastEventId, stop.signal)) {
          failures = 0;
          await supervisor.handle(m);
          if (m.id) {
            lastEventId = m.id;
            updateConfig({ lastEventId }, p);
          }
        }
        if (!stop.signal.aborted) log("the stream ended; reconnecting");
      } catch (e) {
        if (stop.signal.aborted) break;
        if (e instanceof UnpairedError) {
          unpaired = true;
          log(e.message);
          break;
        }
        failures++;
        const wait = backoffMs(failures);
        log(`connection problem (${e instanceof HttpError ? `${e.status} ${e.message}` : e.message}); retrying in ${Math.round(wait / 1000)}s`);
        await sleep(wait, stop.signal);
      }
    }
  } finally {
    clearInterval(heartbeat);
    await supervisor.shutdown();
  }
  return { unpaired };
}
