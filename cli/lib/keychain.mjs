// The macOS login Keychain, through the built-in `security` tool. Shared by the CLI (one API key per server) and the daemon
// (its device key). Elsewhere each caller keeps a file only its user can read.

import { execFileSync, spawnSync } from "node:child_process";

export function keychainGet(service, account) {
  try {
    return execFileSync("security", ["find-generic-password", "-s", service, "-a", account, "-w"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim() || null;
  } catch {
    return null;
  }
}

export function keychainSet(service, account, label, secret) {
  // `security -i` reads the command from stdin, so the secret never shows in the process list as an argument would.
  const r = spawnSync("security", ["-i"], { input: `add-generic-password -U -s "${service}" -a "${account}" -l "${label}" -w "${secret}"\n`, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`could not save the key in the Keychain: ${(r.stderr || r.stdout || "").trim()}`);
}

export function keychainDelete(service, account) {
  spawnSync("security", ["delete-generic-password", "-s", service, "-a", account], { stdio: "ignore" });
}
