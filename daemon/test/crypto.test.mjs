import { test } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import { ASSERTION_AUDIENCE, canonicalJson, exportPrivateKey, generateKeyPair, importPrivateKey, makeAssertion, publicKeyFromRaw, rawPublicKey, verifySigned } from "../src/crypto.mjs";

test("canonical JSON is the server's: keys sorted at every level, undefined dropped", () => {
  // The same cases as the server's test (packages/core/src/lib/daemon.test.ts in the Wheelhouse repo).
  assert.equal(canonicalJson({ b: 1, a: { d: [1, { z: 1, y: 2 }], c: undefined }, e: null }), '{"a":{"d":[1,{"y":2,"z":1}]},"b":1,"e":null}');
  assert.equal(canonicalJson("x"), '"x"');
  assert.equal(canonicalJson({ s: "é\n\"" }), '{"s":"é\\n\\""}');
});

test("a signature over the canonical form verifies whatever the key order it arrives in", () => {
  const server = crypto.generateKeyPairSync("ed25519");
  const message = { type: "job.offer", deviceId: "d", jobId: "j", issue: { title: "t", ref: "WH-1" } };
  const signature = crypto.sign(null, Buffer.from(canonicalJson(message)), server.privateKey).toString("base64url");
  const key = publicKeyFromRaw(rawPublicKey(server.publicKey));
  assert.equal(verifySigned(key, JSON.parse(JSON.stringify({ issue: { ref: "WH-1", title: "t" }, jobId: "j", deviceId: "d", type: "job.offer" })), signature), true);
  assert.equal(verifySigned(key, { ...message, jobId: "other" }, signature), false);
  assert.equal(verifySigned(key, message, "garbage"), false);
});

test("the assertion is an EdDSA JWT for the token endpoint, signed by the device key, two minutes long", () => {
  const { privateKey, publicKey } = generateKeyPair();
  const again = importPrivateKey(exportPrivateKey(privateKey));
  const jwt = makeAssertion("device-1", again, 1_700_000_000_000);
  const [h, p, s] = jwt.split(".");
  assert.deepEqual(JSON.parse(Buffer.from(h, "base64url")), { alg: "EdDSA", typ: "JWT" });
  const claims = JSON.parse(Buffer.from(p, "base64url"));
  assert.equal(claims.sub, "device-1");
  assert.equal(claims.aud, ASSERTION_AUDIENCE);
  assert.equal(claims.exp - claims.iat, 120);
  assert.ok(claims.jti.length >= 16);
  assert.ok(crypto.verify(null, Buffer.from(`${h}.${p}`), publicKeyFromRaw(publicKey), Buffer.from(s, "base64url")));
});
