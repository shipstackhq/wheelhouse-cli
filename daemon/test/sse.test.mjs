import { test } from "node:test";
import assert from "node:assert/strict";
import { SseParser } from "../src/connection.mjs";

test("messages with id, event and data, comments skipped, text cut anywhere", () => {
  const p = new SseParser();
  const text = "retry: 5000\n\n: keep-alive\n\nid: 7\nevent: job.offer\ndata: {\"a\":1}\n\nid: 8\nevent: settings.update\ndata: line1\ndata: line2\n\n";
  const out = [];
  for (let i = 0; i < text.length; i += 3) out.push(...p.push(text.slice(i, i + 3)));
  assert.deepEqual(out, [
    { id: "7", event: "job.offer", data: '{"a":1}' },
    { id: "8", event: "settings.update", data: "line1\nline2" },
  ]);
});

test("CRLF line ends, and a message without an event is a plain message", () => {
  const p = new SseParser();
  assert.deepEqual(p.push("data: x\r\n\r\n"), [{ id: null, event: "message", data: "x" }]);
});
