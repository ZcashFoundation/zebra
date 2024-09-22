//! Common functions used in Zebra.

use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::PathBuf,
};

use tempfile::PersistError;

/// Returns Zebra's default cache directory path.
pub fn default_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("cache"))
        .join("zebra")
}

/// Accepts a target file path and a byte-slice.
///
/// Atomically writes the byte-slice to a file to avoid corrupting the file if Zebra
/// panics, crashes, or exits while the file is being written, or if multiple Zebra instances
/// try to read and write the same file.
///
/// Returns the provided file path if successful.
///
/// # Concurrency
///
/// This function blocks on filesystem operations and should be called in a blocking task
/// when calling from an async environment.
///
/// # Panics
///
/// If the provided `file_path` is a directory path.
pub fn atomic_write(
    file_path: PathBuf,
    data: &[u8],
) -> io::Result<Result<PathBuf, PersistError<fs::File>>> {
    // Get the file's parent directory, or use Zebra's default cache directory
    let file_dir = file_path
        .parent()
        .map(|p| p.to_owned())
        .unwrap_or_else(default_cache_dir);

    // Create the directory if needed.
    fs::create_dir_all(&file_dir)?;

    // Give the temporary file a similar name to the permanent file,
    // but hide it in directory listings.
    let mut tmp_file_prefix: OsString = ".tmp.".into();
    tmp_file_prefix.push(
        file_path
            .file_name()
            .expect("file path must have a file name"),
    );

    // Create the temporary file in the same directory as the permanent file,
    // so atomic filesystem operations are possible.
    let mut tmp_file = tempfile::Builder::new()
        .prefix(&tmp_file_prefix)
        .tempfile_in(file_dir)?;

    tmp_file.write_all(data)?;

    // Atomically write the temp file to `file_path`.
    let persist_result = tmp_file
        .persist(&file_path)
        // Drops the temp file and returns the file path.
        .map(|_| file_path);
    Ok(persist_result)
}
