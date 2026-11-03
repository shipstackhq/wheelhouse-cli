//! The device assertion (an EdDSA JWT for `POST /api/daemon/token`) and verification of the server's signatures.

use crate::canonical::{canonical_json, write_string};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::Value;

pub const ASSERTION_AUDIENCE: &str = "wheelhouse:daemon-token";

fn quoted(s: &str) -> String {
    let mut o = String::new();
    write_string(s, &mut o);
    o
}

/// Same bytes as `makeAssertion` in Node for the same inputs: header and claims in that key order, no spaces.
pub fn make_assertion_with(device_id: &str, key: &SigningKey, now_ms: u64, jti: &str) -> String {
    let iat = now_ms / 1000;
    let header = r#"{"alg":"EdDSA","typ":"JWT"}"#;
    let claims = format!(
        r#"{{"sub":{},"aud":{},"iat":{iat},"exp":{},"jti":{}}}"#,
        quoted(device_id),
        quoted(ASSERTION_AUDIENCE),
        iat + 120,
        quoted(jti)
    );
    let head = format!("{}.{}", URL_SAFE_NO_PAD.encode(header), URL_SAFE_NO_PAD.encode(claims));
    let sig = key.sign(head.as_bytes());
    format!("{head}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
}

pub fn make_assertion(device_id: &str, key: &SigningKey, now_ms: u64) -> String {
    make_assertion_with(device_id, key, now_ms, &uuid::Uuid::new_v4().to_string())
}

/// Whether `signature` (base64url) is the server's signature over the canonical JSON of `message`.
pub fn verify_signed(server_key: &VerifyingKey, message: &Value, signature: &str) -> bool {
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(signature.trim_end_matches('=')) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(&bytes) else {
        return false;
    };
    server_key.verify(canonical_json(message).as_bytes(), &sig).is_ok()
}

/// Sign the canonical form of a value (used by tests and fixtures; the server does this in production).
pub fn sign_value(key: &SigningKey, message: &Value) -> String {
    URL_SAFE_NO_PAD.encode(key.sign(canonical_json(message).as_bytes()).to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;
    use serde_json::json;

    #[test]
    fn sign_verify_any_key_order() {
        let k = keys::generate();
        let sig = sign_value(&k, &json!({"b": 1, "a": [1, 2]}));
        assert!(verify_signed(&k.verifying_key(), &json!({"a": [1, 2], "b": 1}), &sig));
        assert!(!verify_signed(&k.verifying_key(), &json!({"a": [1, 2], "b": 2}), &sig));
        assert!(!verify_signed(&k.verifying_key(), &json!({}), "not a signature"));
    }

    #[test]
    fn assertion_shape() {
        let k = keys::generate();
        let a = make_assertion_with("dev-1", &k, 1_700_000_123_456, "j-1");
        let parts: Vec<&str> = a.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims = String::from_utf8(URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(
            claims,
            r#"{"sub":"dev-1","aud":"wheelhouse:daemon-token","iat":1700000123,"exp":1700000243,"jti":"j-1"}"#
        );
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        k.verifying_key().verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).unwrap();
    }
}
