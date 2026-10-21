// What this machine can run right now, as the daemon reports it on every connect. Everything comes from asking the claude CLI itself
// (`claude --version`, `claude auth status`), never from reading its files, and only the fields Wheelhouse uses are sent: no email,
// no organisation.

import { execFile } from "node:child_process";

/** The model aliases every Claude Code install understands; the plan decides which ones actually answer. */
export const MODELS = ["opus", "sonnet", "haiku"];

function out(cmd, args, timeout = 15_000) {
  return new Promise((resolve) => {
    execFile(cmd, args, { encoding: "utf8", timeout }, (err, stdout) => resolve(err ? null : stdout.trim()));
  });
}

export async function probeClaude(claudePath = "claude") {
  const version = await out(claudePath, ["--version"]);
  if (version == null) return { installed: false, version: null, signedIn: false, subscriptionType: null, models: [] };
  let status = null;
  try {
    status = JSON.parse((await out(claudePath, ["auth", "status", "--json"])) ?? "null");
  } catch {
    /* an older CLI without `auth status`: treat as unknown */
  }
  const signedIn = status?.loggedIn === true;
  return {
    installed: true,
    version: /^[\d.]+/.exec(version)?.[0] ?? version.slice(0, 40),
    signedIn,
    subscriptionType: signedIn && typeof status?.subscriptionType === "string" ? status.subscriptionType : null,
    models: signedIn ? MODELS : [],
  };
}

/** On a Mac, whether it is running on battery (`pmset`); false elsewhere. */
export async function onBattery() {
  if (process.platform !== "darwin") return false;
  return /Battery Power/.test((await out("pmset", ["-g", "batt"], 5000)) ?? "");
}

export async function capabilities({ claudePath, heldRepositories }) {
  return {
    claude: await probeClaude(claudePath),
    repositories: heldRepositories.map((repositoryId) => ({ repositoryId, source: "clone" })),
    platform: process.platform,
    power: { onBattery: await onBattery() },
  };
}
