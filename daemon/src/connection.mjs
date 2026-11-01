// The daemon's line to Wheelhouse: it always dials out. Messages come down one SSE stream (read with fetch, because EventSource cannot
// send an Authorization header); everything the daemon reports goes up as plain POSTs. Requests carry a 15-minute token bought by
// signing an assertion with the device key, renewed before it runs out.

import { makeAssertion } from "./crypto.mjs";

export class HttpError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

/** The device has been unpaired (or its key is not the one Wheelhouse knows): nothing will work until it is paired again. */
export class UnpairedError extends Error {}

/** A request that gets no answer in this long is given up on (a connection that died quietly, after the laptop slept, never answers). */
export const REQUEST_TIMEOUT_MS = 30_000;
/** The server sends a keep-alive every 25 seconds; a stream silent for this long is dead, and is dropped so the daemon reconnects. */
export const STREAM_IDLE_MS = 75_000;

export class Connection {
  constructor({ server, deviceId, privateKey, fetch = globalThis.fetch, now = Date.now, requestTimeoutMs = REQUEST_TIMEOUT_MS, streamIdleMs = STREAM_IDLE_MS }) {
    this.requestTimeoutMs = requestTimeoutMs;
    this.streamIdleMs = streamIdleMs;
    this.server = server.replace(/\/+$/, "");
    this.deviceId = deviceId;
    this.privateKey = privateKey;
    this.fetch = fetch;
    this.now = now;
    this.cached = null; // { token, expiresAt }
  }

  async token() {
    if (this.cached && this.cached.expiresAt - this.now() > 60_000) return this.cached.token;
    const res = await this.fetch(`${this.server}/api/daemon/token`, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ assertion: makeAssertion(this.deviceId, this.privateKey, this.now()) }),
      signal: AbortSignal.timeout(this.requestTimeoutMs),
    });
    const body = await res.json().catch(() => null);
    if (res.status === 401) throw new UnpairedError("Wheelhouse no longer accepts this device's key; it was probably unpaired");
    if (!res.ok) throw new HttpError(res.status, body?.error?.code ?? "http_error", body?.error?.message ?? `token request failed (${res.status})`);
    this.cached = { token: body.accessToken, expiresAt: this.now() + body.expiresIn * 1000 };
    return this.cached.token;
  }

  /** A JSON request with the device token. A 401 (the token was dropped server side) buys a fresh token and tries once more. */
  async request(method, path, body, retried = false) {
    const res = await this.fetch(`${this.server}${path}`, {
      method,
      headers: { authorization: `Bearer ${await this.token()}`, accept: "application/json", ...(body !== undefined ? { "content-type": "application/json" } : {}) },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(this.requestTimeoutMs),
    });
    const json = await res.json().catch(() => null);
    if (res.status === 401 && !retried) {
      this.cached = null;
      return this.request(method, path, body, true);
    }
    if (!res.ok) throw new HttpError(res.status, json?.error?.code ?? "http_error", json?.error?.message ?? `${method} ${path} failed (${res.status})`);
    return json;
  }

  post(path, body = {}) {
    return this.request("POST", path, body);
  }

  /**
   * Open the stream once and yield its messages ({ id, event, data }) until it ends or `signal` aborts. Reconnecting is the caller's
   * job: it knows the last id it handled.
   */
  async *stream(lastEventId, signal) {
    // One controller for the connection: it ends when the caller aborts, or when nothing (not even a keep-alive) arrives for too long.
    const conn = new AbortController();
    const onAbort = () => conn.abort();
    signal?.addEventListener("abort", onAbort, { once: true });
    let idle = null;
    let wentQuiet = false;
    const arm = () => {
      clearTimeout(idle);
      idle = setTimeout(() => ((wentQuiet = true), conn.abort()), this.streamIdleMs);
    };
    try {
      arm();
      const res = await this.fetch(`${this.server}/api/daemon/stream`, {
        headers: { authorization: `Bearer ${await this.token()}`, accept: "text/event-stream", ...(lastEventId != null ? { "last-event-id": String(lastEventId) } : {}) },
        signal: conn.signal,
      });
      if (res.status === 401) {
        this.cached = null;
        throw new HttpError(401, "unauthorized", "the stream refused the token");
      }
      if (!res.ok || !res.body) throw new HttpError(res.status, "http_error", `stream failed (${res.status})`);
      const parser = new SseParser();
      const decoder = new TextDecoder();
      for await (const chunk of res.body) {
        arm();
        for (const m of parser.push(decoder.decode(chunk, { stream: true }))) yield m;
      }
    } catch (e) {
      if (wentQuiet && !signal?.aborted) throw new Error(`the stream went quiet for ${Math.round(this.streamIdleMs / 1000)}s`);
      throw e;
    } finally {
      clearTimeout(idle);
      signal?.removeEventListener("abort", onAbort);
      conn.abort();
    }
  }
}

/**
 * Server-sent events, parsed by hand: `event:`, `data:` (several lines join with newlines), `id:`; a blank line ends a message;
 * a line starting with `:` is a comment (the keep-alive). Text may arrive cut anywhere, so a partial line waits for the next chunk.
 */
export class SseParser {
  constructor() {
    this.buffer = "";
    this.reset();
  }
  reset() {
    this.event = "message";
    this.data = [];
    this.id = null;
  }
  push(text) {
    this.buffer += text;
    const out = [];
    let nl;
    while ((nl = this.buffer.search(/\r\n|\n|\r/)) >= 0) {
      const line = this.buffer.slice(0, nl);
      this.buffer = this.buffer.slice(nl + (this.buffer.startsWith("\r\n", nl) ? 2 : 1));
      if (line === "") {
        if (this.data.length) out.push({ id: this.id, event: this.event, data: this.data.join("\n") });
        this.reset();
        continue;
      }
      if (line.startsWith(":")) continue;
      const i = line.indexOf(":");
      const field = i < 0 ? line : line.slice(0, i);
      const value = i < 0 ? "" : line.slice(i + 1).replace(/^ /, "");
      if (field === "event") this.event = value;
      else if (field === "data") this.data.push(value);
      else if (field === "id") this.id = value;
    }
    return out;
  }
}

/**
 * A job's progress on its way up: events are numbered here (the server stores each number once, so a resent batch is harmless) and
 * sent in batches, every quarter second or every 50 events. A failed send keeps the events and tries again later; past 2,000
 * waiting, the oldest are dropped so a long outage cannot eat memory.
 */
export class JobReporter {
  constructor(conn, jobId, { path = `/api/daemon/jobs/${jobId}`, flushMs = 250, batch = 50, maxQueued = 2000, finishRetryMs = 1000, log = () => {} } = {}) {
    this.conn = conn;
    this.jobId = jobId;
    this.path = path; // where this run's events and finish go: a job's, or a chat message's
    this.flushMs = flushMs;
    this.batch = batch;
    this.maxQueued = maxQueued;
    this.finishRetryMs = finishRetryMs;
    this.log = log;
    this.seq = 0;
    this.queue = [];
    this.timer = null;
    this.sending = null;
    this.failures = 0;
  }

  event(type, data = {}) {
    this.queue.push({ seq: this.seq++, at: new Date().toISOString(), type, data });
    if (this.queue.length > this.maxQueued) this.queue.splice(0, this.queue.length - this.maxQueued);
    if (this.queue.length >= this.batch) void this.flush();
    else if (!this.timer) this.timer = setTimeout(() => void this.flush(), this.flushMs);
  }

  async flush() {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    if (this.sending) return this.sending;
    if (!this.queue.length) return;
    const batch = this.queue.slice(0, 200);
    this.sending = (async () => {
      try {
        await this.conn.post(`${this.path}/events`, { events: batch });
        this.queue.splice(0, batch.length);
        this.failures = 0;
      } catch (e) {
        this.failures++;
        this.log(`job ${this.jobId}: could not send progress (${e.message}); will retry`);
        // A finished job's events are refused for good: stop trying.
        if (e instanceof HttpError && (e.status === 409 || e.status === 404)) this.queue = [];
      } finally {
        this.sending = null;
      }
      if (this.queue.length) this.timer = setTimeout(() => void this.flush(), Math.min(30_000, this.flushMs * 2 ** this.failures));
    })();
    return this.sending;
  }

  /**
   * Send what is left (a few tries), then the final state. Returns `{ sent, retry }`: `retry` is true when the finish did not get
   * through for a reason that may pass (no connection, a server error), so the caller keeps it and sends it again later.
   */
  async finish(result) {
    for (let i = 0; i < 5 && this.queue.length; i++) await this.flush();
    if (this.timer) clearTimeout(this.timer);
    let lastError;
    for (let i = 0; i < 5; i++) {
      try {
        await this.conn.post(`${this.path}/finish`, result);
        return { sent: true, retry: false };
      } catch (e) {
        lastError = e;
        if (e instanceof HttpError && e.status < 500 && e.status !== 429) return (this.log(`job ${this.jobId}: could not report the finish (${e.message})`), { sent: false, retry: false });
        await new Promise((r) => setTimeout(r, this.finishRetryMs * 2 ** i));
      }
    }
    this.log(`job ${this.jobId}: could not report the finish (${lastError?.message}); keeping it to send when Wheelhouse is reachable`);
    return { sent: false, retry: true };
  }
}
