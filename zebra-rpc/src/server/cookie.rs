//! Cookie-based authentication for the RPC server.

use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use color_eyre::Result;
use rand::RngCore;

use std::{
    fs::{remove_file, File},
    io::{Read, Write},
    path::PathBuf,
};

/// The user field in the cookie (arbitrary, only for recognizability in debugging/logging purposes)
pub const COOKIEAUTH_USER: &str = "__cookie__";
/// Default name for auth cookie file */
pub const COOKIEAUTH_FILE: &str = ".cookie";

/// Generate a new auth cookie and store it in the given `cookie_dir`.
pub fn generate(cookie_dir: PathBuf) -> Result<()> {
    let mut data = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut data);
    let encoded_password = URL_SAFE.encode(data);
    let cookie_content = format!("{}:{}", COOKIEAUTH_USER, encoded_password);

    let mut file = File::create(cookie_dir.join(COOKIEAUTH_FILE))?;
    file.write_all(cookie_content.as_bytes())?;

    tracing::info!("RPC auth cookie generated successfully");

    Ok(())
}

/// Get the encoded password from the auth cookie.
pub fn get(cookie_dir: PathBuf) -> Result<String> {
    let mut file = File::open(cookie_dir.join(COOKIEAUTH_FILE))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let parts: Vec<&str> = contents.split(":").collect();
    Ok(parts[1].to_string())
}

/// Delete the auth cookie.
pub fn delete() -> Result<()> {
    remove_file(COOKIEAUTH_FILE)?;
    tracing::info!("RPC auth cookie deleted successfully");
    Ok(())
}
