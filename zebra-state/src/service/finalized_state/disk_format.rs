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

/// Helper trait for defining the exact format used to store to disk,
/// for each type.
pub trait IntoDisk {
    /// The type used to compare a value as a key to other keys stored in a
    /// database.
    type Bytes: AsRef<[u8]>;

    /// Converts the current type to its disk format in `zs_get()`,
    /// without necessarily allocating a new ivec.
    fn as_bytes(&self) -> Self::Bytes;
}

/// Helper trait for types with fixed-length disk storage.
///
/// This trait must not be implemented for types with variable-length disk storage.
pub trait IntoDiskFixedLen: IntoDisk {
    /// Returns the fixed serialized length of `Bytes`.
    fn fixed_byte_len() -> usize;
}

/// Helper type for retrieving types from the disk with the correct format.
///
/// The ivec should be correctly encoded by IntoDisk.
pub trait FromDisk: Sized {
    /// Function to convert the disk bytes back into the deserialized type.
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
    fn fixed_byte_len() -> usize {
        // Bytes is probably a [u8; N]
        Self::Bytes::default().into_iter().count()
    }
}
