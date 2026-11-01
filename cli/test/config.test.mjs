import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "bin", "wheelhouse.mjs");
let home, repo;

before(() => {
  home = fs.mkdtempSync(path.join(os.tmpdir(), "wh-config-home-"));
  repo = fs.mkdtempSync(path.join(os.tmpdir(), "wh-config-repo-"));
  spawnSync("git", ["init", "-q"], { cwd: repo });
  fs.writeFileSync(path.join(repo, "wheelhouse.json"), JSON.stringify({ url: "http://repo.example", team: "QQ" }));
});
after(() => {
  fs.rmSync(home, { recursive: true, force: true });
  fs.rmSync(repo, { recursive: true, force: true });
});

const whoami = (env = {}, args = []) =>
  spawnSync("node", [script, "whoami", ...args], { cwd: repo, encoding: "utf8", env: { PATH: process.env.PATH, HOME: home, WHEELHOUSE_KEYSTORE: "file", ...env } }).stdout;
const plugin = { CLAUDE_PLUGIN_OPTION_SERVER_URL: "http://plugin.example", CLAUDE_PLUGIN_OPTION_API_KEY: "wh_plugin" };

test("keys saved before 0.2.0 move from ~/.config/wheelhouse to ~/.wheelhouse on first use", () => {
  const old = path.join(home, ".config", "wheelhouse", "credentials.json");
  fs.mkdirSync(path.dirname(old), { recursive: true });
  fs.writeFileSync(old, JSON.stringify({ "http://repo.example": "wh_saved" }));
  assert.match(whoami(), /key:\s+wh_save… {2}\(.*\.wheelhouse\/credentials\.json\)/);
  assert.ok(!fs.existsSync(old));
  assert.equal(fs.statSync(path.join(home, ".wheelhouse", "credentials.json")).mode & 0o777, 0o600);
});

test("wheelhouse.json and the saved key win over the Claude Code plugin's options", () => {
  const out = whoami(plugin);
  assert.match(out, /server:\s+http:\/\/repo\.example {2}\(wheelhouse\.json\)/);
  assert.match(out, /key:\s+wh_save…/);
});

test("the plugin's options are the fallback, and WHEELHOUSE_* and flags come first", () => {
  fs.rmSync(path.join(home, ".wheelhouse", "credentials.json"));
  assert.match(whoami(plugin), /key:\s+wh_plug… {2}\(plugin\)/);
  assert.match(whoami({ ...plugin, WHEELHOUSE_KEY: "wh_envkey" }), /key:\s+wh_envk… {2}\(WHEELHOUSE_KEY\)/);
  assert.match(whoami({ WHEELHOUSE_TEAM: "EN" }, ["--team", "FL"]), /team:\s+FL {2}\(--team\)/);
});
