//! Wire-level pieces shared by the CLI and the daemon: canonical JSON, Ed25519 keys, the device assertion, signature checks,
//! the SSE parser and the server message types. Everything here must agree byte-for-byte with the Node implementation.

pub mod canonical;
pub mod keys;
pub mod messages;
pub mod sign;
pub mod sse;

pub use canonical::canonical_json;
pub use sign::{make_assertion, verify_signed, ASSERTION_AUDIENCE};
pub use sse::{SseMessage, SseParser};
