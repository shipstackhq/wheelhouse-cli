//! OS-specific code. So far: the macOS login Keychain, through the built-in `security` tool, exactly as `cli/lib/keychain.mjs` does
//! (same commands, so a key saved by the Node daemon is read by this one and the other way round). Service install and paths land here too.

pub mod keychain;
