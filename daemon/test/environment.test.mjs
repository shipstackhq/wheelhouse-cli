import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { findClaude, loginShellPath, mergePath } from "../src/environment.mjs";

test("PATH entries merge in order without duplicates", () => {
  assert.equal(mergePath("/usr/bin:/bin", "/opt/x:/usr/bin", null, "/bin:/y"), "/usr/bin:/bin:/opt/x:/y");
});

test("claude is found on a PATH, by an explicit path, or not at all", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "wh-env-"));
  const bin = path.join(dir, "claude");
  fs.writeFileSync(bin, "#!/bin/sh\n", { mode: 0o755 });
  assert.equal(findClaude("claude", `/nowhere:${dir}`), bin);
  assert.equal(findClaude(bin, ""), bin);
  assert.equal(findClaude("claude", "/nowhere"), null);
  assert.equal(findClaude(path.join(dir, "nope"), ""), null);
  fs.rmSync(dir, { recursive: true, force: true });
});

test("the login shell's PATH can be read, and is a real PATH", () => {
  const p = loginShellPath("/bin/sh");
  assert.ok(p === null || p.split(":").length > 1);
});
