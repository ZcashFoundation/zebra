//! Serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::sync::Arc;

pub mod block;
pub mod chain;
pub mod shielded;
pub mod transparent;

#[cfg(test)]
mod tests;

pub use block::TransactionLocation;

/// Helper type for writing types to disk as raw bytes.
/// Also used to convert key types to raw bytes for disk lookups.
pub trait IntoDisk {
    /// The type used to write bytes to disk,
    /// and compare a value as a key to on-disk keys.
    type Bytes: AsRef<[u8]>;

    /// Converts the current type into serialized raw bytes.
    ///
    /// Used to convert keys to bytes in [`ReadDisk`],
    /// and keys and values to bytes in [`WriteDisk`].
    ///
    /// # Panics
    ///
    /// - if the input data doesn't serialize correctly
    fn as_bytes(&self) -> Self::Bytes;
}

/// Helper trait for types with fixed-length disk storage.
///
/// This trait must not be implemented for types with variable-length disk storage.
pub trait IntoDiskFixedLen: IntoDisk {
    /// Returns the fixed serialized length of `Bytes`.
    fn fixed_disk_byte_len() -> usize;
}

/// Helper type for reading types from disk as raw bytes.
pub trait FromDisk: Sized {
    /// Converts raw disk bytes back into the deserialized type.
    ///
    /// Used to convert keys and values from bytes in [`ReadDisk`].
    ///
    /// # Panics
    ///
    /// - if the input data doesn't deserialize correctly
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self;
}

// Generic serialization impls

impl<'a, T> IntoDisk for &'a T
where
    T: IntoDisk,
{
    type Bytes = T::Bytes;

    fn as_bytes(&self) -> Self::Bytes {
        T::as_bytes(*self)
    }
}

impl<T> IntoDisk for Arc<T>
where
    T: IntoDisk,
{
    type Bytes = T::Bytes;

    fn as_bytes(&self) -> Self::Bytes {
        T::as_bytes(&*self)
    }
}

impl<T> FromDisk for Arc<T>
where
    T: FromDisk,
{
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Arc::new(T::from_bytes(bytes))
    }
}

// Commonly used serialization impls

impl IntoDisk for () {
    type Bytes = [u8; 0];

    fn as_bytes(&self) -> Self::Bytes {
        []
    }
}

// Generic serialization length impls

impl<T> IntoDiskFixedLen for T
where
    T: IntoDisk,
    T::Bytes: Default + IntoIterator + Copy,
{
    /// Returns the fixed size of `Bytes`.
    ///
    /// Assumes that `Copy` types are fixed-sized byte arrays.
    fn fixed_disk_byte_len() -> usize {
        // Bytes is probably a [u8; N]
        Self::Bytes::default().into_iter().count()
    }
}

// Serialization Modification Functions

/// Truncate `mem_bytes` to `disk_len`, by removing zero bytes from the start of the slice.
/// Used to discard unused zero bytes during serialization.
///
/// # Panics
///
/// - if `mem_bytes` is shorter than `disk_len`
/// - if any of the truncated bytes are non-zero
pub fn truncate_zero_be_bytes(mem_bytes: &[u8], disk_len: usize) -> &[u8] {
    let truncated_bytes = mem_bytes
        .len()
        .checked_sub(disk_len)
        .expect("unexpected `mem_bytes` length: must be at least `disk_len`");

    if truncated_bytes == 0 {
        return mem_bytes;
    }

    let (discarded, truncated) = mem_bytes.split_at(truncated_bytes);

    assert!(
        discarded.iter().all(|&byte| byte == 0),
        "unexpected `mem_bytes` content: non-zero discarded bytes: {:?}\n\
         truncated: {:?}",
        discarded,
        truncated,
    );

    assert_eq!(truncated.len(), disk_len);

    truncated
}

/// Expand `disk_bytes` to `mem_len`, by adding zero bytes at the start of the slice.
/// Used to zero-fill bytes that were discarded during serialization.
///
/// # Panics
///
/// - if `disk_bytes` is longer than `mem_len`
pub fn expand_zero_be_bytes(disk_bytes: &[u8], mem_len: usize) -> Vec<u8> {
    let extra_bytes = mem_len
        .checked_sub(disk_bytes.len())
        .expect("unexpected `disk_bytes` length: must not exceed `mem_len`");

    if extra_bytes == 0 {
        return disk_bytes.to_vec();
    }

    let mut expanded = vec![0; extra_bytes];
    expanded.extend(disk_bytes);

    assert_eq!(expanded.len(), mem_len);

    expanded
}
