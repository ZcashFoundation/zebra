//! Cookie-based authentication for the RPC server.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use color_eyre::Result;
use rand::RngCore;

use std::{
    fs::{remove_file, File},
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// The name of the cookie file on the disk
const FILE: &str = ".cookie";

/// The maximum number of times Zebra retries after temporary cookie file name collisions.
const TEMPORARY_COOKIE_FILE_CREATE_RETRIES: usize = 10;

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
pub fn write_to_disk(cookie: &Cookie, dir: &Path, file_name: Option<&str>) -> Result<()> {
    std::fs::create_dir_all(dir)?;

    let cookie_path = dir.join(file_name.unwrap_or(FILE));

    if cookie_path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(color_eyre::eyre::eyre!(
            "cookie path {cookie_path:?} is a symlink, refusing to write"
        ));
    }

    write_owner_only_file(&cookie_path, format!("__cookie__:{}", cookie.0).as_bytes())?;

    tracing::info!("RPC auth cookie written to disk");

    Ok(())
}

/// Writes the given contents to a file readable and writable only by the owner.
///
/// Creating a fresh temporary file first avoids reusing permissions from a
/// pre-existing cookie file.
fn write_owner_only_file(path: &Path, contents: &[u8]) -> Result<()> {
    for _ in 0..TEMPORARY_COOKIE_FILE_CREATE_RETRIES {
        let temp_path = temporary_cookie_path(path);

        let mut file = match create_owner_only_file(&temp_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };

        file.write_all(contents)?;
        drop(file);

        if let Err(error) = std::fs::rename(&temp_path, path) {
            let _ = remove_file(temp_path);
            return Err(error.into());
        }

        return Ok(());
    }

    Err(color_eyre::eyre::eyre!(
        "failed to create a unique temporary cookie file for {path:?}"
    ))
}

/// Returns a random temporary path next to the cookie file.
fn temporary_cookie_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|file_name| file_name.to_string_lossy())
        .unwrap_or_else(|| FILE.into());
    let random_suffix = rand::thread_rng().next_u64();

    path.with_file_name(format!("{file_name}.{random_suffix:x}.tmp"))
}

/// Creates a file readable and writable only by the owner.
///
/// On Unix, this sets mode 0600 regardless of umask.
/// On Windows, default ACLs already restrict access to the creating user,
/// so no explicit hardening is needed.
fn create_owner_only_file(path: &Path) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);

    #[cfg(unix)]
    opts.mode(0o600);

    opts.open(path)
}

/// Removes a cookie from the given dir.
pub fn remove_from_disk(dir: &Path, file_name: Option<&str>) -> Result<()> {
    remove_file(dir.join(file_name.unwrap_or(FILE)))?;

    tracing::info!("RPC auth cookie removed from disk");

    Ok(())
}
