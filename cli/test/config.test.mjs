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

test("integrations lists Claude Code, marks it installed from Claude's plugin list, and runs its hook", () => {
  const run = (args) => spawnSync("node", [script, ...args], { cwd: repo, input: "{}", encoding: "utf8", env: { PATH: process.env.PATH, HOME: home, WHEELHOUSE_KEYSTORE: "file" } });
  assert.match(run(["integrations"]).stdout, /^· claude-code/);
  const plugins = path.join(home, ".claude", "plugins", "installed_plugins.json");
  fs.mkdirSync(path.dirname(plugins), { recursive: true });
  fs.writeFileSync(plugins, JSON.stringify({ plugins: { "wheelhouse@shipstack": [{}] } }));
  assert.match(run(["integrations"]).stdout, /^✓ claude-code/);
  assert.equal(run(["integrations", "claude-code", "hook", "session-end"]).status, 0);
  assert.equal(run(["integrations", "nope"]).status, 1);
  assert.match(run(["integrations", "claude-code"]).stderr, /Usage: wheelhouse integrations claude-code <hook \| install>/);
});

test("doctor names the fix for each problem and exits non-zero when something is broken", () => {
  const r = spawnSync("node", [script, "doctor"], { cwd: repo, encoding: "utf8", env: { PATH: process.env.PATH, HOME: home, WHEELHOUSE_KEYSTORE: "file" } });
  assert.equal(r.status, 1);
  assert.match(r.stdout, /✗ key: none for http:\/\/repo\.example\n {4}→ wheelhouse login --url http:\/\/repo\.example/);
  assert.match(r.stdout, /✓ repo linked to team QQ/);
  assert.match(r.stdout, /✗ git hooks missing/);
  assert.match(r.stdout, /· daemon: not paired/);
});
