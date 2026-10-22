import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { DEFAULT_RUNNER, loadConfig, paths, saveConfig } from "../src/config.mjs";

test("only settings that differ from the defaults are saved, so new defaults reach an existing machine", () => {
  const p = paths(fs.mkdtempSync(path.join(os.tmpdir(), "wh-config-")));
  saveConfig({ ...loadConfig(p), server: "https://wh.example", poolSize: 2 }, p);
  assert.deepEqual(JSON.parse(fs.readFileSync(p.config, "utf8")), { server: "https://wh.example", poolSize: 2 });
  assert.deepEqual(loadConfig(p).runner, DEFAULT_RUNNER);
  fs.rmSync(p.home, { recursive: true, force: true });
});
