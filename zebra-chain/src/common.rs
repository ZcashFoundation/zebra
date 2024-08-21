//! Common functions used in Zebra.

use std::{ffi::OsString, io, path::PathBuf};

use tempfile::{NamedTempFile, PersistError};
use tokio::io::AsyncWriteExt as _;
use tracing::Span;

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
/// We want to use async code to avoid blocking the tokio executor on filesystem operations,
/// but `tempfile` is implemented using non-asyc methods. So we wrap its filesystem
/// operations in `tokio::spawn_blocking()`.
///
/// # Panics
///
/// If the provided `file_path` is a directory path.
pub async fn atomic_write_to_tmp_file(
    file_path: PathBuf,
    data: &[u8],
) -> io::Result<Result<PathBuf, PersistError<tokio::fs::File>>> {
    // Get the file's parent directory, or use Zebra's default cache directory
    let file_dir = file_path
        .parent()
        .map(|p| p.to_owned())
        .unwrap_or_else(default_cache_dir);

    // Create the directory if needed.
    tokio::fs::create_dir_all(&file_dir).await?;

    // Give the temporary file a similar name to the permanent file,
    // but hide it in directory listings.
    let mut tmp_file_prefix: OsString = ".tmp.".into();
    tmp_file_prefix.push(
        file_path
            .file_name()
            .expect("file path must have a file name"),
    );

    // Create the temporary file.
    // Do blocking filesystem operations on a dedicated thread.
    let span = Span::current();
    let tmp_file = tokio::task::spawn_blocking(move || {
        span.in_scope(move || {
            // Put the temporary file in the same directory as the permanent file,
            // so atomic filesystem operations are possible.
            tempfile::Builder::new()
                .prefix(&tmp_file_prefix)
                .tempfile_in(file_dir)
        })
    })
    .await
    .expect("unexpected panic creating temporary file")?;

    // Write data to the file asynchronously, by extracting the inner file, using it,
    // then combining it back into a type that will correctly drop the file on error.
    let (tmp_file, tmp_path) = tmp_file.into_parts();
    let mut tmp_file = tokio::fs::File::from_std(tmp_file);
    tmp_file.write_all(data).await?;

    let tmp_file = NamedTempFile::from_parts(tmp_file, tmp_path);

    // Atomically replace the current file with the temporary file.
    // Do blocking filesystem operations on a dedicated thread.
    let span = Span::current();
    let persist_result = tokio::task::spawn_blocking(move || {
        span.in_scope(move || {
            tmp_file
                .persist(&file_path)
                // Drops the temp file and returns the file path if needed.
                .map(|_| file_path)
        })
    })
    .await
    .expect("unexpected panic making temporary file permanent");

    Ok(persist_result)
}
