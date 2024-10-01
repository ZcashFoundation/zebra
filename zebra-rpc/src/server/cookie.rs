//! Cookie-based authentication for the RPC server.

use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use rand::RngCore;

use std::{
    fs::{remove_file, File},
    io::{Read, Write},
};

/// The user field in the cookie (arbitrary, only for recognizability in debugging/logging purposes)
pub const COOKIEAUTH_USER: &str = "__cookie__";
/// Default name for auth cookie file */
const COOKIEAUTH_FILE: &str = ".cookie";

/// Generate a new auth cookie and return the encoded password.
pub fn generate() -> Option<()> {
    let mut data = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut data);
    let encoded_password = URL_SAFE.encode(data);
    let cookie_content = format!("{}:{}", COOKIEAUTH_USER, encoded_password);

    let mut file = File::create(COOKIEAUTH_FILE).ok()?;
    file.write_all(cookie_content.as_bytes()).ok()?;

    tracing::info!("RPC auth cookie generated successfully");

    Some(())
}

/// Get the encoded password from the auth cookie.
pub fn get() -> Option<String> {
    let mut file = File::open(COOKIEAUTH_FILE).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;

    let parts: Vec<&str> = contents.split(":").collect();
    Some(parts[1].to_string())
}

/// Delete the auth cookie.
pub fn delete() -> Option<()> {
    remove_file(COOKIEAUTH_FILE).ok()?;
    tracing::info!("RPC auth cookie deleted successfully");
    Some(())
}
