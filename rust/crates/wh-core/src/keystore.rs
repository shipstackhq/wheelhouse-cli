//! Where the device's private key is kept. On macOS: the login Keychain, with the same service and account names as the Node daemon so
//! a paired machine can swap daemons without pairing again. Elsewhere (and when the config says "file", which tests use): a file only
//! this user can read. Port of `daemon/src/keystore.mjs`.

use crate::config::write_private;
use std::path::{Path, PathBuf};

pub const SERVICE: &str = "com.shipstack.wheelhouse-daemon";
pub const ACCOUNT: &str = "device-key";

fn key_file(home: &Path) -> PathBuf {
    home.join("device-key")
}

pub fn store_key(kind: &str, home: &Path, secret: &str) -> Result<(), String> {
    if kind == "keychain" {
        return wh_platform::keychain::set(SERVICE, ACCOUNT, "Wheelhouse daemon", secret);
    }
    std::fs::create_dir_all(home).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700));
    }
    write_private(&key_file(home), format!("{secret}\n").as_bytes()).map_err(|e| e.to_string())
}

pub fn load_key(kind: &str, home: &Path) -> Option<String> {
    if kind == "keychain" {
        return wh_platform::keychain::get(SERVICE, ACCOUNT);
    }
    let s = std::fs::read_to_string(key_file(home)).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

pub fn delete_key(kind: &str, home: &Path) {
    if kind == "keychain" {
        wh_platform::keychain::delete(SERVICE, ACCOUNT);
    } else {
        let _ = std::fs::remove_file(key_file(home));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_and_account_names_are_the_node_daemons() {
        assert_eq!(
            (SERVICE, ACCOUNT),
            ("com.shipstack.wheelhouse-daemon", "device-key")
        );
    }

    #[test]
    fn a_file_key_is_stored_private_read_back_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("h");
        assert_eq!(load_key("file", &home), None);
        store_key("file", &home, "secret").unwrap();
        assert_eq!(load_key("file", &home).as_deref(), Some("secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(key_file(&home))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        delete_key("file", &home);
        assert_eq!(load_key("file", &home), None);
    }
}
