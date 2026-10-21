#!/usr/bin/env node
// wheelhouse-daemon: pair this machine with Wheelhouse, and run the daemon.
//
//   wheelhouse-daemon pair --server https://wheelhouse.example [--name "Atharva's MacBook Pro"]
//   wheelhouse-daemon run          run in the foreground (what the background service runs)
//   wheelhouse-daemon status       what this machine is paired with
//   wheelhouse-daemon unpair       forget the pairing and the key here (unpair it in Wheelhouse too)
//
// Zero dependencies (Node >= 18).

import { execFile } from "node:child_process";
import os from "node:os";
import { DEFAULT_CONFIG, loadConfig, paths, saveConfig } from "./config.mjs";
import { exportPrivateKey, generateKeyPair } from "./crypto.mjs";
import { log, runDaemon, VERSION } from "./daemon.mjs";
import { deleteKey, loadKey, storeKey } from "./keystore.mjs";

function args(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      const [k, v] = a.slice(2).split("=", 2);
      out[k] = v ?? (argv[i + 1] && !argv[i + 1].startsWith("--") ? argv[++i] : true);
    } else out._.push(a);
  }
  return out;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function postJson(url, body) {
  const res = await fetch(url, { method: "POST", headers: { "content-type": "application/json", accept: "application/json" }, body: JSON.stringify(body) });
  const json = await res.json().catch(() => null);
  if (!res.ok) throw new Error(json?.error?.message ?? `${url} answered ${res.status}`);
  return json;
}

/** A name for the machine people recognise: the Mac's own "computer name" when there is one, else the host name. */
async function machineName() {
  if (process.platform === "darwin") {
    const name = await new Promise((r) => execFile("scutil", ["--get", "ComputerName"], { encoding: "utf8" }, (e, out) => r(e ? null : out.trim())));
    if (name) return name;
  }
  return os.hostname().replace(/\.local$/, "");
}

async function pair(o) {
  const server = String(o.server ?? "").replace(/\/+$/, "");
  if (!/^https?:\/\//.test(server)) throw new Error("usage: wheelhouse-daemon pair --server https://your-wheelhouse [--name NAME]");
  const p = paths();
  const before = loadConfig(p);
  if (before.deviceId && !o.force) throw new Error(`Already paired with ${before.server} as ${before.name}. Run \`wheelhouse-daemon unpair\` first, or pass --force.`);
  const keystore = o.keystore === "file" ? "file" : before.keystore;
  const name = typeof o.name === "string" ? o.name : await machineName();

  const keys = generateKeyPair();
  const start = await postJson(`${server}/api/daemon/pair/start`, { publicKey: keys.publicKey, name, os: process.platform });
  console.log(`\nTo pair ${name} with Wheelhouse, open:\n\n  ${start.verificationUriComplete}\n\nand check that it shows this code: ${start.userCode}\n`);
  if (process.platform === "darwin" && !o["no-open"]) execFile("open", [start.verificationUriComplete], () => {});

  let interval = start.interval * 1000;
  const deadline = Date.now() + start.expiresIn * 1000;
  while (Date.now() < deadline) {
    await sleep(interval);
    const r = await postJson(`${server}/api/daemon/pair/poll`, { deviceCode: start.deviceCode });
    if (r.status === "pending") continue;
    if (r.status === "slow_down") {
      interval += 5000;
      continue;
    }
    if (r.status === "denied") throw new Error("Pairing was refused in Wheelhouse.");
    if (r.status === "expired") break;
    if (r.status === "approved") {
      storeKey(keystore, p.home, exportPrivateKey(keys.privateKey));
      saveConfig({ ...before, server, keystore, deviceId: r.deviceId, serverPublicKey: r.serverPublicKey, workspace: r.workspace, name: r.device.name, poolSize: r.device.poolSize, lastEventId: null }, p);
      console.log(`Paired as "${r.device.name}" in ${r.workspace.name}. The key is in the ${keystore === "keychain" ? "login Keychain" : `file ${p.home}/device-key`}.`);
      console.log(`Start it now with \`wheelhouse-daemon run\`, or install it as a background service:\n  sh daemon/platform/macos/install.sh --entry "$(pwd)/daemon/src/main.mjs"`);
      return;
    }
  }
  throw new Error("The code expired before it was approved. Run pair again.");
}

function status() {
  const p = paths();
  const c = loadConfig(p);
  if (!c.deviceId) return console.log(`Not paired. Settings would live in ${p.home}.`);
  const key = loadKey(c.keystore, p.home);
  console.log(
    [
      `wheelhouse-daemon ${VERSION}`,
      `server:     ${c.server}`,
      `workspace:  ${c.workspace?.name ?? "?"}`,
      `device:     ${c.name} (${c.deviceId})`,
      `runs:       up to ${c.poolSize} at once`,
      `key:        ${key ? `in the ${c.keystore}` : `MISSING from the ${c.keystore}`}`,
      `home:       ${p.home}`,
    ].join("\n"),
  );
}

function unpair() {
  const p = paths();
  const c = loadConfig(p);
  deleteKey(c.keystore, p.home);
  saveConfig({ ...DEFAULT_CONFIG, keystore: c.keystore, claudePath: c.claudePath, gitProtocol: c.gitProtocol, runner: c.runner, repositories: c.repositories }, p);
  console.log(`Forgot the pairing${c.name ? ` of ${c.name}` : ""} and its key. Unpair it in Wheelhouse (Settings → Devices) as well, if it is still listed.`);
}

async function main() {
  const o = args(process.argv.slice(2));
  const cmd = o._[0];
  if (cmd === "pair") return pair(o);
  if (cmd === "status") return status();
  if (cmd === "unpair") return unpair();
  if (cmd === "run") {
    const stop = new AbortController();
    for (const s of ["SIGTERM", "SIGINT"]) process.once(s, () => (log(`${s}: stopping, canceling running jobs`), stop.abort()));
    const { unpaired } = await runDaemon({ signal: stop.signal });
    // Unpaired is a clean exit, so the background service does not restart it; anything else is a failure and is restarted.
    process.exit(unpaired ? 0 : stop.signal.aborted ? 0 : 1);
  }
  if (cmd === "--version" || cmd === "version") return console.log(VERSION);
  console.log("usage: wheelhouse-daemon pair --server <url> [--name NAME] | run | status | unpair");
  process.exitCode = cmd ? 2 : 0;
}

main().catch((e) => {
  console.error(e.message);
  process.exit(1);
});
