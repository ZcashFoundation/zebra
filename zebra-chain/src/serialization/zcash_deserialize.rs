use std::io;

use super::{ReadZcashExt, SerializationError};
use byteorder::ReadBytesExt;

/// Consensus-critical serialization for Zcash.
///
/// This trait provides a generic deserialization for consensus-critical
/// formats, such as network messages, transactions, blocks, etc. It is intended
/// for use only in consensus-critical contexts; in other contexts, such as
/// internal storage, it would be preferable to use Serde.
pub trait ZcashDeserialize: Sized {
    /// Try to read `self` from the given `reader`.
    ///
    /// This function has a `zcash_` prefix to alert the reader that the
    /// serialization in use is consensus-critical serialization, rather than
    /// some other kind of serialization.
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError>;
}

impl<T: ZcashDeserialize> ZcashDeserialize for Vec<T> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let len = reader.read_compactsize()?;
        // We're given len, so we could preallocate. But blindly preallocating
        // without a size bound can allow DOS attacks, and there's no way to
        // pass a size bound in a ZcashDeserialize impl, so instead we allocate
        // as we read from the reader. (The maximum block and transaction sizes
        // limit the eventual size of these allocations.)
        let mut vec = Vec::new();
        for _ in 0..len {
            vec.push(T::zcash_deserialize(&mut reader)?);
        }
        Ok(vec)
    }
}

/// Read a byte.
impl ZcashDeserialize for u8 {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(reader.read_u8()?)
    }
}

/// Read a Bitcoin-encoded UTF-8 string.
impl ZcashDeserialize for String {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        let bytes: Vec<_> = Vec::zcash_deserialize(reader)?;
        String::from_utf8(bytes).map_err(|_| SerializationError::Parse("invalid utf-8"))
    }
}

/// Helper for deserializing more succinctly via type inference
pub trait ZcashDeserializeInto {
    /// Deserialize based on type inference
    fn zcash_deserialize_into<T>(self) -> Result<T, SerializationError>
    where
        T: ZcashDeserialize;
}

impl<R: io::Read> ZcashDeserializeInto for R {
    fn zcash_deserialize_into<T>(self) -> Result<T, SerializationError>
    where
        T: ZcashDeserialize,
    {
        T::zcash_deserialize(self)
    }
}
