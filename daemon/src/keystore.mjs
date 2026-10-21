// Where the device's private key is kept. On macOS: the login Keychain, through the `security` tool. Elsewhere (and when the config
// says "file", which tests use): a file only this user can read. The key is written once, at pairing, and read at every start.

import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const SERVICE = "com.shipstack.wheelhouse-daemon";
const ACCOUNT = "device-key";

function keyFile(home) {
  return path.join(home, "device-key");
}

export function storeKey(kind, home, secret) {
  if (kind === "keychain") {
    // `security -i` reads the command from stdin, so the key never appears in the process list as an argument would.
    const r = spawnSync("security", ["-i"], { input: `add-generic-password -U -s "${SERVICE}" -a "${ACCOUNT}" -l "Wheelhouse daemon" -w "${secret}"\n`, encoding: "utf8" });
    if (r.status !== 0) throw new Error(`could not save the key in the Keychain: ${(r.stderr || r.stdout || "").trim()}`);
    return;
  }
  fs.mkdirSync(home, { recursive: true, mode: 0o700 });
  fs.writeFileSync(keyFile(home), secret + "\n", { mode: 0o600 });
}

export function loadKey(kind, home) {
  if (kind === "keychain") {
    try {
      return execFileSync("security", ["find-generic-password", "-s", SERVICE, "-a", ACCOUNT, "-w"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim() || null;
    } catch {
      return null;
    }
  }
  try {
    return fs.readFileSync(keyFile(home), "utf8").trim() || null;
  } catch {
    return null;
  }
}

export function deleteKey(kind, home) {
  if (kind === "keychain") {
    spawnSync("security", ["delete-generic-password", "-s", SERVICE, "-a", ACCOUNT], { stdio: "ignore" });
    return;
  }
  fs.rmSync(keyFile(home), { force: true });
}
