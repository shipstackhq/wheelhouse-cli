// Ed25519 between the daemon and Wheelhouse. The daemon proves who it is by signing a short assertion with its own key (the private
// key never leaves this machine), and checks every message from the server against the server key it pinned when it paired.
// Signatures cover the canonical JSON of a value (keys sorted at every level, no spaces): the server computes it the same way.

import crypto from "node:crypto";

export const ASSERTION_AUDIENCE = "wheelhouse:daemon-token";

export function canonicalJson(v) {
  if (v === null || typeof v !== "object") return JSON.stringify(v) ?? "null";
  if (Array.isArray(v)) return `[${v.map((x) => (x === undefined ? "null" : canonicalJson(x))).join(",")}]`;
  const keys = Object.keys(v)
    .filter((k) => v[k] !== undefined)
    .sort();
  return `{${keys.map((k) => `${JSON.stringify(k)}:${canonicalJson(v[k])}`).join(",")}}`;
}

export function generateKeyPair() {
  const { privateKey, publicKey } = crypto.generateKeyPairSync("ed25519");
  return { privateKey, publicKey: rawPublicKey(publicKey) };
}

export const rawPublicKey = (k) => k.export({ format: "jwk" }).x;
export const publicKeyFromRaw = (raw) => crypto.createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: raw }, format: "jwk" });
export const exportPrivateKey = (k) => k.export({ format: "der", type: "pkcs8" }).toString("base64");
export const importPrivateKey = (b64) => crypto.createPrivateKey({ key: Buffer.from(b64, "base64"), format: "der", type: "pkcs8" });

/** An EdDSA JWT saying "I am this device", good for two minutes and once (the jti), for POST /api/daemon/token. */
export function makeAssertion(deviceId, privateKey, now = Date.now()) {
  const iat = Math.floor(now / 1000);
  const b64 = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
  const head = `${b64({ alg: "EdDSA", typ: "JWT" })}.${b64({ sub: deviceId, aud: ASSERTION_AUDIENCE, iat, exp: iat + 120, jti: crypto.randomUUID() })}`;
  return `${head}.${crypto.sign(null, Buffer.from(head), privateKey).toString("base64url")}`;
}

/** Whether `signature` is the server's signature over `message`. */
export function verifySigned(serverKey, message, signature) {
  try {
    return crypto.verify(null, Buffer.from(canonicalJson(message)), serverKey, Buffer.from(String(signature), "base64url"));
  } catch {
    return false;
  }
}
