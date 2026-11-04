// Runs the Node daemon's crypto and writes what Rust must reproduce: rust/fixtures/crypto.json.
// Usage: node rust/tools/gen-fixtures.mjs   (CI runs it before the Rust tests; the output is not committed)
//
// The server's own signing code is not in this repository, so server-side fixtures are produced here with the daemon's
// canonicalJson (which the daemon documents as identical to the server's) and a stand-in server key. If the server's signing
// module becomes available, add its output to `serverSigned` below.

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { ASSERTION_AUDIENCE, canonicalJson, exportPrivateKey, importPrivateKey, makeAssertion, rawPublicKey, verifySigned, publicKeyFromRaw } from "../../daemon/src/crypto.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.join(here, "..", "fixtures", "crypto.json");

// A key from a fixed seed, so the fixtures are stable between runs (Ed25519 signatures are deterministic).
const keyFromSeed = (byte) => {
  const der = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.alloc(32, byte)]);
  return importPrivateKey(der.toString("base64"));
};
const device = keyFromSeed(7);
const server = keyFromSeed(9);

// Inputs are JSON text so Rust reads exactly what Node read. `undefined` cases are built in JS and given as their wire form.
const withUndefined = { b: undefined, a: 1, c: [undefined, 2, { z: undefined, y: 1 }] };
const cases = [
  [null, "null"],
  [true, "true"],
  [0, "0"],
  [-0, "-0"],
  [1e21, "1e21"],
  [1e-7, "1e-7"],
  [123456789012345680000, "123456789012345680000"],
  [0.1 + 0.2, "0.30000000000000004"],
  [2 ** 53 + 2, "9007199254740994"],
  ["a\"b\\c\n\r\t\b\f\u0001\u001f\u007f é😀", JSON.stringify("a\"b\\c\n\r\t\b\f\u0001\u001f\u007f é😀")],
  [{ b: 1, a: { z: 1, y: [3, 2, 1] }, "\u{10000}": 1, "￿": 2, "": 3, B: 4, 10: 5, 9: 6 }, '{"b":1,"a":{"z":1,"y":[3,2,1]},"\u{10000}":1,"￿":2,"":3,"B":4,"10":5,"9":6}'],
  [withUndefined, '{"a":1,"c":[null,2,{"y":1}]}'],
  [[], "[]"],
  [{}, "{}"],
].map(([value, json]) => ({ json, canonical: canonicalJson(value) }));

const messages = [
  { type: "job.offer", jobId: "j1", deviceId: "dev-1", expiresAt: "2030-01-01T00:00:00.000Z", branch: "wh/x", repository: { id: "r", cloneUrl: "https://x/y.git" }, issue: { ref: "WH-1" }, ratio: 0.5, big: 1e21 },
  { type: "settings.update", deviceId: "dev-1", poolSize: 3, chatModel: null },
  { type: "chat.message", deviceId: "dev-1", messageId: "m1", prompt: "héllo \"world\"\n", threadId: "t" },
];
const serverSigned = messages.map((m) => {
  const signature = crypto.sign(null, Buffer.from(canonicalJson(m)), server).toString("base64url");
  if (!verifySigned(publicKeyFromRaw(rawPublicKey(crypto.createPublicKey(server))), m, signature)) throw new Error("self-check failed");
  return { message: JSON.stringify(m), canonical: canonicalJson(m), signature };
});

const now = 1_700_000_123_456;
const assertion = makeAssertion("dev-1", device, now);

fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(
  out,
  JSON.stringify(
    {
      audience: ASSERTION_AUDIENCE,
      device: { privateKey: exportPrivateKey(device), publicKey: rawPublicKey(crypto.createPublicKey(device)) },
      server: { privateKey: exportPrivateKey(server), publicKey: rawPublicKey(crypto.createPublicKey(server)) },
      canonical: cases,
      serverSigned,
      assertion: { deviceId: "dev-1", now, value: assertion },
    },
    null,
    2,
  ),
);
console.log(`wrote ${out}`);
