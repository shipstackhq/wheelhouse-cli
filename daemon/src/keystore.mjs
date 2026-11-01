// Where the device's private key is kept. On macOS: the login Keychain (cli/lib/keychain.mjs). Elsewhere (and when the config
// says "file", which tests use): a file only this user can read. The key is written once, at pairing, and read at every start.

import { keychainDelete, keychainGet, keychainSet } from "../../cli/lib/keychain.mjs";
import fs from "node:fs";
import path from "node:path";

const SERVICE = "com.shipstack.wheelhouse-daemon";
const ACCOUNT = "device-key";

function keyFile(home) {
  return path.join(home, "device-key");
}

export function storeKey(kind, home, secret) {
  if (kind === "keychain") return keychainSet(SERVICE, ACCOUNT, "Wheelhouse daemon", secret);
  fs.mkdirSync(home, { recursive: true, mode: 0o700 });
  fs.writeFileSync(keyFile(home), secret + "\n", { mode: 0o600 });
}

export function loadKey(kind, home) {
  if (kind === "keychain") return keychainGet(SERVICE, ACCOUNT);
  try {
    return fs.readFileSync(keyFile(home), "utf8").trim() || null;
  } catch {
    return null;
  }
}

export function deleteKey(kind, home) {
  if (kind === "keychain") return keychainDelete(SERVICE, ACCOUNT);
  fs.rmSync(keyFile(home), { force: true });
}
