//! Ed25519 keys in the formats the Node daemon stores: private as base64 PKCS8 DER, public as raw base64url (no padding).

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use ed25519_dalek::{SigningKey, VerifyingKey};

#[derive(Debug)]
pub struct KeyError(pub String);

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid key: {}", self.0)
    }
}
impl std::error::Error for KeyError {}

pub fn generate() -> SigningKey {
    SigningKey::generate(&mut rand::rngs::OsRng)
}

pub fn export_private(k: &SigningKey) -> String {
    let der = k.to_pkcs8_der().expect("pkcs8 encoding of an ed25519 key");
    STANDARD.encode(der.as_bytes())
}

pub fn import_private(b64: &str) -> Result<SigningKey, KeyError> {
    let der = STANDARD.decode(b64.trim()).map_err(|e| KeyError(e.to_string()))?;
    SigningKey::from_pkcs8_der(&der).map_err(|e| KeyError(e.to_string()))
}

pub fn raw_public(k: &VerifyingKey) -> String {
    URL_SAFE_NO_PAD.encode(k.as_bytes())
}

pub fn public_from_raw(raw: &str) -> Result<VerifyingKey, KeyError> {
    let bytes = URL_SAFE_NO_PAD.decode(raw.trim_end_matches('=')).map_err(|e| KeyError(e.to_string()))?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| KeyError("public key must be 32 bytes".into()))?;
    VerifyingKey::from_bytes(&arr).map_err(|e| KeyError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_public_round_trip() {
        let k = generate();
        let k2 = import_private(&export_private(&k)).unwrap();
        assert_eq!(k.to_bytes(), k2.to_bytes());
        let raw = raw_public(&k.verifying_key());
        assert!(!raw.contains('=') && !raw.contains('+') && !raw.contains('/'));
        assert_eq!(public_from_raw(&raw).unwrap(), k.verifying_key());
    }
}
