//! Cookie-based authentication for the RPC server.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use color_eyre::Result;
use rand::RngCore;

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
    pub fn authenticate(&self, passwd: String) -> bool {
        *passwd == self.0
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
