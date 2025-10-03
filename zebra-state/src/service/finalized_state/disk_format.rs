//! Serialization formats for finalized data.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::{io::Write, sync::Arc};

pub mod block;
pub mod chain;
pub mod shielded;
pub mod transparent;
pub mod upgrade;

#[cfg(any(test, feature = "proptest-impl"))]
mod tests;

pub use block::{TransactionIndex, TransactionLocation, MAX_ON_DISK_HEIGHT};
pub use transparent::OutputLocation;

#[cfg(any(test, feature = "proptest-impl"))]
pub use tests::KV;

/// Helper type for writing types to disk as raw bytes.
/// Also used to convert key types to raw bytes for disk lookups.
pub trait IntoDisk {
    /// The type used to write bytes to disk,
    /// and compare a value as a key to on-disk keys.
    type Bytes: AsRef<[u8]>;

    /// Converts the current type into serialized raw bytes.
    ///
    /// Used to convert keys to bytes in [`ReadDisk`][1],
    /// and keys and values to bytes in [`WriteDisk`][2].
    ///
    /// # Panics
    ///
    /// - if the input data doesn't serialize correctly
    ///
    /// [1]: super::disk_db::ReadDisk
    /// [2]: super::disk_db::WriteDisk
    fn as_bytes(&self) -> Self::Bytes;
}

/// Helper type for reading types from disk as raw bytes.
pub trait FromDisk: Sized {
    /// Converts raw disk bytes back into the deserialized type.
    ///
    /// Used to convert keys and values from bytes in [`ReadDisk`][1].
    ///
    /// # Panics
    ///
    /// - if the input data doesn't deserialize correctly
    ///
    /// [1]: super::disk_db::ReadDisk
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self;
}

// Generic serialization impls

impl<T> IntoDisk for &T
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
        T::as_bytes(self)
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

impl FromDisk for () {
    #[allow(clippy::unused_unit)]
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        assert_eq!(
            bytes.as_ref().len(),
            0,
            "unexpected data in zero-sized column family type",
        );

        ()
    }
}

/// Access database keys or values as raw bytes.
/// Mainly for use in tests, runtime checks, or format compatibility code.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct RawBytes(Vec<u8>);

// Note: don't implement From or Into for RawBytes, because it makes it harder to spot in reviews.
// Instead, implement IntoDisk and FromDisk on the original type, or a specific wrapper type.

impl RawBytes {
    /// Create a new raw byte key or data.
    ///
    /// Mainly for use in tests or runtime checks.
    /// These methods
    pub fn new_raw_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Create a new raw byte key or data.
    /// Mainly for use in tests.
    pub fn raw_bytes(&self) -> &Vec<u8> {
        &self.0
    }
}

impl IntoDisk for RawBytes {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.raw_bytes().clone()
    }
}

impl FromDisk for RawBytes {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Self::new_raw_bytes(bytes.as_ref().to_vec())
    }
}

// Serialization Modification Functions

/// Truncates `mem_bytes` to `disk_len`, by removing zero bytes from the start of the slice.
/// Used to discard unused zero bytes during serialization.
///
/// Return `None` if any of the truncated bytes are non-zero
///
/// # Panics
///
/// - if `mem_bytes` is shorter than `disk_len`.
pub fn truncate_zero_be_bytes(mem_bytes: &[u8], disk_len: usize) -> Option<&[u8]> {
    let discarded_bytes = mem_bytes
        .len()
        .checked_sub(disk_len)
        .expect("unexpected `mem_bytes` length: must be at least `disk_len`");

    if discarded_bytes == 0 {
        return Some(mem_bytes);
    }

    let (discarded, truncated) = mem_bytes.split_at(discarded_bytes);

    if !discarded.iter().all(|&byte| byte == 0) {
        return None;
    }

    assert_eq!(truncated.len(), disk_len);

    Some(truncated)
}

/// Expands `disk_bytes` to `MEM_SIZE`, by adding zero bytes at the start of the slice.
/// Used to zero-fill bytes that were discarded during serialization.
///
/// # Panics
///
/// - if `disk_bytes` is longer than `mem_len`
#[inline]
pub fn expand_zero_be_bytes<const MEM_SIZE: usize>(disk_bytes: &[u8]) -> [u8; MEM_SIZE] {
    // Return early if disk_bytes is already the expected length
    if let Ok(disk_bytes_array) = disk_bytes.try_into() {
        return disk_bytes_array;
    }

    let extra_bytes = MEM_SIZE
        .checked_sub(disk_bytes.len())
        .expect("unexpected `disk_bytes` length: must not exceed `MEM_SIZE`");

    let mut expanded = [0; MEM_SIZE];
    let _ = (&mut expanded[extra_bytes..])
        .write(disk_bytes)
        .expect("should fit");

    expanded
}
