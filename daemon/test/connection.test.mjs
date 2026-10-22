import { test } from "node:test";
import assert from "node:assert/strict";
import { Connection } from "../src/connection.mjs";
import { generateKeyPair } from "../src/crypto.mjs";

const { privateKey } = generateKeyPair();
const json = (status, body) => ({ status, ok: status < 400, json: async () => body });

/** A fetch that answers the token exchange and hands the rest to `handler`. */
const fakeFetch = (handler) => async (url, init) => (url.endsWith("/api/daemon/token") ? json(200, { accessToken: "whd_x", expiresIn: 900 }) : handler(url, init));

test("a request that is never answered is given up on instead of hanging", async () => {
  const conn = new Connection({ server: "http://x", deviceId: "d", privateKey, requestTimeoutMs: 50, fetch: fakeFetch((_u, init) => new Promise((_r, reject) => init.signal.addEventListener("abort", () => reject(init.signal.reason)))) });
  await assert.rejects(conn.post("/api/daemon/heartbeat", {}), (e) => e.name === "TimeoutError" || e.name === "AbortError");
});

test("a stream that goes quiet is dropped with a clear error, so the daemon reconnects", async () => {
  // Like a real response body, it errors when the request is aborted.
  const quiet = (_u, init) => ({
    status: 200,
    ok: true,
    body: new ReadableStream({
      start(c) {
        c.enqueue(new TextEncoder().encode("retry: 5000\n\n"));
        init.signal.addEventListener("abort", () => c.error(init.signal.reason));
      },
    }),
  });
  const conn = new Connection({ server: "http://x", deviceId: "d", privateKey, streamIdleMs: 60, fetch: fakeFetch(quiet) });
  await assert.rejects(async () => { for await (const _ of conn.stream(null, new AbortController().signal)); }, /went quiet/);
});

test("a stream that keeps receiving keep-alives stays open, and the caller can still end it", async () => {
  let push;
  const body = new ReadableStream({ start(c) { push = (t) => c.enqueue(new TextEncoder().encode(t)); } });
  const conn = new Connection({ server: "http://x", deviceId: "d", privateKey, streamIdleMs: 80, fetch: fakeFetch((_u, init) => ({ status: 200, ok: true, body: new ReadableStream({ start(c) { init.signal.addEventListener("abort", () => c.error(init.signal.reason)); body.pipeTo(new WritableStream({ write: (v) => c.enqueue(v) })).catch(() => {}); } }) })) });
  const stop = new AbortController();
  const got = [];
  const run = (async () => { try { for await (const m of conn.stream(null, stop.signal)) got.push(m); } catch (e) { return e; } })();
  for (let i = 0; i < 4; i++) { push(": keep-alive\n\n"); await new Promise((r) => setTimeout(r, 40)); }
  push("id: 1\nevent: settings.update\ndata: {}\n\n");
  await new Promise((r) => setTimeout(r, 40));
  stop.abort();
  const e = await run;
  assert.deepEqual(got.map((m) => m.id), ["1"]);
  assert.ok(!e || !/went quiet/.test(e.message), "an abort by the caller is not reported as quiet");
});
