// A background service starts with a bare PATH (launchd gives /usr/bin:/bin:/usr/sbin:/sbin), which hides the tools a person's shell
// finds: claude in ~/.local/bin, gh (often git's credential helper) and pnpm in Homebrew or nvm folders. At start the daemon takes
// the PATH of the user's login shell, plus the usual install folders, so it and every run see what a terminal sees.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const COMMON = () => [path.join(os.homedir(), ".local", "bin"), path.join(os.homedir(), ".claude", "local"), "/opt/homebrew/bin", "/usr/local/bin"];

/** The PATH an interactive login shell of the user would have, or null if it cannot be read in a few seconds. */
export function loginShellPath(shell = process.env.SHELL || "/bin/zsh") {
  try {
    const out = execFileSync(shell, ["-ilc", 'printf "__WH_PATH__%s__WH_END__" "$PATH"'], { encoding: "utf8", timeout: 5000, stdio: ["ignore", "pipe", "ignore"] });
    return /__WH_PATH__(.*)__WH_END__/s.exec(out)?.[1] || null;
  } catch {
    return null;
  }
}

/** PATH entries joined without duplicates, the current ones first. */
export function mergePath(...parts) {
  const seen = new Set();
  return parts
    .flatMap((p) => (p ? p.split(path.delimiter) : []))
    .filter((d) => d && !seen.has(d) && seen.add(d))
    .join(path.delimiter);
}

/** Widen process.env.PATH for this process and its children. */
export function adoptUserPath() {
  process.env.PATH = mergePath(process.env.PATH, loginShellPath(), COMMON().join(path.delimiter));
  return process.env.PATH;
}

/** The claude executable: the configured path if it is one, else the first `claude` on PATH. Null when there is none. */
export function findClaude(configured = "claude", searchPath = process.env.PATH ?? "") {
  if (configured.includes("/")) return fs.existsSync(configured) ? configured : null;
  for (const dir of searchPath.split(path.delimiter)) {
    const p = path.join(dir, configured);
    try {
      fs.accessSync(p, fs.constants.X_OK);
      return p;
    } catch {
      /* not here */
    }
  }
  return null;
}
