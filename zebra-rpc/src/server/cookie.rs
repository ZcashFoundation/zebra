//! Cookie-based authentication for the RPC server.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use color_eyre::Result;
use rand::RngCore;

use std::{
    fs::{remove_file, File},
    io::Write,
    path::Path,
};

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
pub fn write_to_disk(cookie: &Cookie, dir: &Path) -> Result<()> {
    // Create the directory if needed.
    std::fs::create_dir_all(dir)?;
    File::create(dir.join(FILE))?.write_all(format!("__cookie__:{}", cookie.0).as_bytes())?;

    tracing::info!("RPC auth cookie written to disk");

    Ok(())
}

/// Removes a cookie from the given dir.
pub fn remove_from_disk(dir: &Path) -> Result<()> {
    remove_file(dir.join(FILE))?;

    tracing::info!("RPC auth cookie removed from disk");

    Ok(())
}
