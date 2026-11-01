// The self-check: before a run's branch is pushed, the repository's own check commands (lint, typecheck, tests…) run in its worktree.
// Each one ends as passed, failed (the command ran and exited non-zero) or could_not_run (it never got to run: no such tool, a bad
// directory, a timeout, a stop). Nothing is skipped silently, so "lint was never run" cannot read as "lint passed".

import { execFile } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { scrub } from "./runner/claude.mjs";

export const MAX_FIX_ATTEMPTS = 2;
const TIMEOUT_MS = 10 * 60_000;
const TAIL = 3000;

/** One check: `{ name, command, cwd?, required }` in `dir`. Never throws. */
export function runCheck(check, dir, signal) {
  const started = Date.now();
  const result = (status, exitCode, output) => ({
    name: check.name,
    command: check.command,
    required: check.required !== false,
    status,
    exitCode,
    durationMs: Date.now() - started,
    tail: scrub(String(output ?? "").trim().slice(-TAIL)),
  });
  const cwd = path.resolve(dir, check.cwd ?? ".");
  if (cwd !== dir && !cwd.startsWith(dir + path.sep)) return Promise.resolve(result("could_not_run", null, "The check's directory is outside the worktree"));
  if (!fs.existsSync(cwd)) return Promise.resolve(result("could_not_run", null, `The check's directory ${check.cwd} does not exist`));
  return new Promise((resolve) => {
    execFile("/bin/sh", ["-c", check.command], { cwd, signal, timeout: TIMEOUT_MS, maxBuffer: 16 * 1024 * 1024, encoding: "utf8" }, (err, stdout, stderr) => {
      const output = `${stdout}${stderr}`;
      if (!err) return resolve(result("passed", 0, output));
      // 127 and 126: the shell could not find or execute the command. A killed process (stop, timeout) has no numeric code.
      if (typeof err.code !== "number" || err.code === 126 || err.code === 127) return resolve(result("could_not_run", typeof err.code === "number" ? err.code : null, output || err.message));
      resolve(result("failed", err.code, output));
    });
  });
}

/** Every check in order, each reported. `onResult` gets each as it ends. */
export async function runChecks(checks, dir, signal, onResult = () => {}) {
  const out = [];
  for (const c of checks) {
    const r = await runCheck(c, dir, signal);
    onResult(r);
    out.push(r);
  }
  return out;
}

/** The check that decides the run, if any: the first required one that did not pass. */
export const blocking = (results) => results.find((c) => c.required && c.status !== "passed");
