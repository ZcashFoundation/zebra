//! Cookie-based authentication for the RPC server.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use color_eyre::Result;
use rand::RngCore;
use subtle::ConstantTimeEq;

use std::{
    fs::{remove_file, File},
    io::Write,
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// The name of the cookie file on the disk
const FILE: &str = ".cookie";

/// If the RPC authentication is enabled, all requests must contain this cookie.
#[derive(Clone, Debug)]
pub struct Cookie(String);

impl Cookie {
    /// Checks if the given passwd matches the contents of the cookie.
    ///
    /// The comparison is constant-time over the cookie length so that a
    /// network-side adversary cannot recover the cookie byte-by-byte by
    /// observing how long the equality check takes. The early-exit short
    /// circuit on a length mismatch is intentional: only the cookie *contents*
    /// are secret, not its length (which is fixed by `Cookie::default`).
    pub fn authenticate(&self, passwd: String) -> bool {
        if passwd.len() != self.0.len() {
            return false;
        }
        passwd.as_bytes().ct_eq(self.0.as_bytes()).into()
    }
}

impl Default for Cookie {
    fn default() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);

        Self(STANDARD.encode(bytes))
    }
}

/// Writes the given cookie to the given dir.
///
/// Uses restrictive file permissions (0600 on Unix) to prevent other
/// local users from reading the cookie secret.
pub fn write_to_disk(cookie: &Cookie, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;

    let cookie_path = dir.join(FILE);

    if cookie_path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(color_eyre::eyre::eyre!(
            "cookie path {cookie_path:?} is a symlink, refusing to write"
        ));
    }

    let mut file = create_owner_only_file(&cookie_path)?;
    file.write_all(format!("__cookie__:{}", cookie.0).as_bytes())?;

    tracing::info!("RPC auth cookie written to disk");

    Ok(())
}

/// Creates a file readable and writable only by the owner.
///
/// On Unix, this sets mode 0600 regardless of umask.
/// On Windows, default ACLs already restrict access to the creating user,
/// so no explicit hardening is needed.
fn create_owner_only_file(path: &Path) -> Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);

    #[cfg(unix)]
    opts.mode(0o600);

    Ok(opts.open(path)?)
}

/// Removes a cookie from the given dir.
pub fn remove_from_disk(dir: &Path) -> Result<()> {
    remove_file(dir.join(FILE))?;

    tracing::info!("RPC auth cookie removed from disk");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper that constructs a `Cookie` from a known string, bypassing
    /// `Default` so the test value is reproducible.
    fn cookie_from(s: &str) -> Cookie {
        Cookie(s.to_string())
    }

    #[test]
    fn authenticate_accepts_exact_match() {
        let cookie = cookie_from("correct-cookie");
        assert!(cookie.authenticate("correct-cookie".to_string()));
    }

    #[test]
    fn authenticate_rejects_wrong_content_same_length() {
        let cookie = cookie_from("correct-cookie");
        // Same length, different content — the length short-circuit must not
        // accept this; the constant-time comparison must reject it.
        assert!(!cookie.authenticate("CORRECT-COOKIE".to_string()));
        assert!(!cookie.authenticate("xxxxxxx-cookie".to_string()));
        assert!(!cookie.authenticate("correct-xxxxxx".to_string()));
    }

    #[test]
    fn authenticate_rejects_wrong_length() {
        let cookie = cookie_from("correct-cookie");
        // Shorter and longer guesses must both be rejected without invoking
        // the constant-time comparison on mismatched-length slices.
        assert!(!cookie.authenticate("correct".to_string()));
        assert!(!cookie.authenticate("correct-cookie-extra".to_string()));
        assert!(!cookie.authenticate(String::new()));
    }

    #[test]
    fn authenticate_rejects_prefix_only_match() {
        // Regression guard for the timing-oracle class: a guess that matches
        // the cookie's prefix but differs at a later byte must be rejected
        // exactly like any other wrong guess.
        let cookie = cookie_from("0123456789abcdef0123456789abcdef0123456789a");
        let mut guess = "0123456789abcdef0123456789abcdef0123456789a".to_string();
        guess.replace_range(40..41, "X");
        assert!(!cookie.authenticate(guess));
    }
}
