//! Rust against Node: fixtures written by `rust/tools/gen-fixtures.mjs`, and a property test over random JSON.
//! Needs `node` on PATH and the fixtures generated first (CI does both; locally: `node rust/tools/gen-fixtures.mjs`).

use proptest::prelude::*;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use wh_proto::{canonical_json, keys, sign, verify_signed};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixtures() -> Value {
    let p = root().join("fixtures/crypto.json");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("missing {}: run `node rust/tools/gen-fixtures.mjs` first", p.display()));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn canonical_bytes_match_node() {
    for c in fixtures()["canonical"].as_array().unwrap() {
        let v: Value = serde_json::from_str(c["json"].as_str().unwrap()).unwrap();
        assert_eq!(canonical_json(&v), c["canonical"].as_str().unwrap(), "input {}", c["json"]);
    }
}

#[test]
fn server_signatures_verify_and_keys_round_trip() {
    let f = fixtures();
    let server = keys::public_from_raw(f["server"]["publicKey"].as_str().unwrap()).unwrap();
    for s in f["serverSigned"].as_array().unwrap() {
        let m: Value = serde_json::from_str(s["message"].as_str().unwrap()).unwrap();
        assert_eq!(canonical_json(&m), s["canonical"].as_str().unwrap());
        assert!(verify_signed(&server, &m, s["signature"].as_str().unwrap()));
        // Ed25519 is deterministic, so signing the same bytes gives Node's signature exactly.
        let sk = keys::import_private(f["server"]["privateKey"].as_str().unwrap()).unwrap();
        assert_eq!(sign::sign_value(&sk, &m), s["signature"].as_str().unwrap());
    }
    let dev = keys::import_private(f["device"]["privateKey"].as_str().unwrap()).unwrap();
    assert_eq!(keys::export_private(&dev), f["device"]["privateKey"].as_str().unwrap());
    assert_eq!(keys::raw_public(&dev.verifying_key()), f["device"]["publicKey"].as_str().unwrap());
}

#[test]
fn assertion_is_byte_identical_to_nodes() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let f = fixtures();
    let a = &f["assertion"];
    let node = a["value"].as_str().unwrap();
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(node.split('.').nth(1).unwrap()).unwrap()).unwrap();
    assert_eq!(claims["aud"], json!(f["audience"]));
    let dev = keys::import_private(f["device"]["privateKey"].as_str().unwrap()).unwrap();
    let mine = sign::make_assertion_with(a["deviceId"].as_str().unwrap(), &dev, a["now"].as_u64().unwrap(), claims["jti"].as_str().unwrap());
    assert_eq!(mine, node);
}

fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-9_007_199_254_740_992i64..9_007_199_254_740_992i64).prop_map(|n| json!(n)),
        any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(|f| json!(f)),
        prop_oneof![(-1e30f64..1e30f64), (-1e-9f64..1e-9f64)].prop_map(|f| json!(f)),
        any::<String>().prop_map(Value::String),
        "[ -~\u{0}-\u{1f}\u{7f}\u{2028}\u{ffff}\u{10000}-\u{10ffff}]{0,12}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec(("[a-zA-Z0-9\u{e9}\u{ffff}\u{10000}-\u{10010}]{0,5}", inner), 0..6)
                .prop_map(|kv| Value::Object(kv.into_iter().collect())),
        ]
    })
}

fn node_canonical(docs: &[String]) -> Vec<String> {
    let mut child = Command::new("node")
        .arg(root().join("tools/canon.mjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("node on PATH");
    let mut stdin = child.stdin.take().unwrap();
    for d in docs {
        writeln!(stdin, "{d}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn random_json_canonicalizes_like_node(docs in prop::collection::vec(arb_json(), 1..12)) {
        // serde_json never puts a raw newline inside a JSON string, so one document per line is safe.
        let texts: Vec<String> = docs.iter().map(|d| serde_json::to_string(d).unwrap()).collect();
        let theirs = node_canonical(&texts);
        prop_assert_eq!(theirs.len(), docs.len());
        for (d, n) in docs.iter().zip(theirs) {
            prop_assert_eq!(canonical_json(d), n);
        }
    }
}
