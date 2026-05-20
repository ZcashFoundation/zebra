//! Tests for cookie file creation security.

use std::fs;

use super::super::cookie;
use crate::server::cookie::Cookie;

#[test]
fn cookie_file_has_restrictive_permissions() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().unwrap();
    let cookie = Cookie::default();

    cookie::write_to_disk(&cookie, dir.path()).unwrap();

    let cookie_path = dir.path().join(".cookie");
    let metadata = fs::metadata(&cookie_path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "cookie file should have mode 0600, got {mode:o}"
        );
    }

    assert!(metadata.len() > 0, "cookie file should not be empty");
}

#[cfg(unix)]
#[test]
fn cookie_write_rejects_symlink() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("decoy");
    fs::write(&target, b"").unwrap();

    std::os::unix::fs::symlink(&target, dir.path().join(".cookie")).unwrap();

    let cookie = Cookie::default();
    let result = cookie::write_to_disk(&cookie, dir.path());
    assert!(result.is_err(), "should reject symlink at cookie path");
}
