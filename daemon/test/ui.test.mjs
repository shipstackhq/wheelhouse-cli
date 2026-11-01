import assert from "node:assert/strict";
import test from "node:test";
import { detectLevel, header } from "../src/ui.mjs";

const tty = { isTTY: true };
const level = (env, platform = "darwin", stream = tty, argv = []) => detectLevel({ env, platform, stream, argv });

test("the output level follows the terminal", () => {
  assert.equal(level({ LANG: "en_US.UTF-8" }), "full");
  assert.equal(level({ LANG: "en_US.UTF-8", TERM: "linux" }, "linux"), "basic");
  assert.equal(level({}, "win32"), "basic");
  assert.equal(level({ WT_SESSION: "1" }, "win32"), "full");
  assert.equal(level({ LANG: "en_US.UTF-8" }, "darwin", { isTTY: false }), "plain");
  assert.equal(level({ LANG: "en_US.UTF-8", NO_COLOR: "" }), "plain");
  assert.equal(level({ LANG: "en_US.UTF-8" }, "darwin", tty, ["--plain"]), "plain");
});

test("the header is one plain line for scripts and one short line on a narrow terminal", () => {
  assert.equal(header({ version: "1.2.3", channel: "beta", level: "plain" }), "wheelhouse daemon · v1.2.3 · beta");
  assert.equal(header({ version: "1.2.3", level: "full", columns: 30 }).split("\n").length, 1);
  assert.equal(header({ version: "1.2.3", level: "basic", columns: 80 }).split("\n").length, 10);
});
