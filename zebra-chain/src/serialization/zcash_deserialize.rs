use std::{
    convert::{TryFrom, TryInto},
    io,
};

use super::{AtLeastOne, ReadZcashExt, SerializationError, MAX_PROTOCOL_MESSAGE_LEN};

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

/// Deserialize a `Vec`, where the number of items is set by a compactsize
/// prefix in the data. This is the most common format in Zcash.
///
/// See `zcash_deserialize_external_count` for more details, and usage
/// information.
impl<T: ZcashDeserialize + TrustedPreallocate> ZcashDeserialize for Vec<T> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let len = reader.read_compactsize()?.try_into()?;
        zcash_deserialize_external_count(len, reader)
    }
}

/// Deserialize an `AtLeastOne` vector, where the number of items is set by a
/// compactsize prefix in the data. This is the most common format in Zcash.
impl<T: ZcashDeserialize + TrustedPreallocate> ZcashDeserialize for AtLeastOne<T> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let v: Vec<T> = (&mut reader).zcash_deserialize_into()?;
        v.try_into()
    }
}

/// Implement ZcashDeserialize for Vec<u8> directly instead of using the blanket Vec implementation
///
/// This allows us to optimize the inner loop into a single call to `read_exact()`
/// Note that we don't implement TrustedPreallocate for u8.
/// This allows the optimization without relying on specialization.
impl ZcashDeserialize for Vec<u8> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let len = reader.read_compactsize()?.try_into()?;
        zcash_deserialize_bytes_external_count(len, reader)
    }
}

/// Deserialize a `Vec` containing `external_count` items.
///
/// In Zcash, most arrays are stored as a compactsize, followed by that number
/// of items of type `T`. But in `Transaction::V5`, some types are serialized as
/// multiple arrays in different locations, with a single compactsize before the
/// first array.
///
/// ## Usage
///
/// Use `zcash_deserialize_external_count` when the array count is determined by
/// other data, or a consensus rule.
///
/// Use `Vec::zcash_deserialize` for data that contains compactsize count,
/// followed by the data array.
///
/// For example, when a single count applies to multiple arrays:
/// 1. Use `Vec::zcash_deserialize` for the array that has a data count.
/// 2. Use `zcash_deserialize_external_count` for the arrays with no count in the
///    data, passing the length of the first array.
///
/// This function has a `zcash_` prefix to alert the reader that the
/// serialization in use is consensus-critical serialization, rather than
/// some other kind of serialization.
pub fn zcash_deserialize_external_count<R: io::Read, T: ZcashDeserialize + TrustedPreallocate>(
    external_count: usize,
    mut reader: R,
) -> Result<Vec<T>, SerializationError> {
    match u64::try_from(external_count) {
        Ok(external_count) if external_count > T::max_allocation() => {
            return Err(SerializationError::Parse(
                "Vector longer than max_allocation",
            ))
        }
        Ok(_) => {}
        // As of 2021, usize is less than or equal to 64 bits on all (or almost all?) supported Rust platforms.
        // So in practice this error is impossible. (But the check is required, because Rust is future-proof
        // for 128 bit memory spaces.)
        Err(_) => return Err(SerializationError::Parse("Vector longer than u64::MAX")),
    }
    let mut vec = Vec::with_capacity(external_count);
    for _ in 0..external_count {
        vec.push(T::zcash_deserialize(&mut reader)?);
    }
    Ok(vec)
}

/// `zcash_deserialize_external_count`, specialised for raw bytes.
///
/// This allows us to optimize the inner loop into a single call to `read_exact()`.
///
/// This function has a `zcash_` prefix to alert the reader that the
/// serialization in use is consensus-critical serialization, rather than
/// some other kind of serialization.
pub fn zcash_deserialize_bytes_external_count<R: io::Read>(
    external_count: usize,
    mut reader: R,
) -> Result<Vec<u8>, SerializationError> {
    if external_count > MAX_U8_ALLOCATION {
        return Err(SerializationError::Parse(
            "Byte vector longer than MAX_U8_ALLOCATION",
        ));
    }
    let mut vec = vec![0u8; external_count];
    reader.read_exact(&mut vec)?;
    Ok(vec)
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

/// Blind preallocation of a Vec<T: TrustedPreallocate> is based on a bounded length. This is in contrast
/// to blind preallocation of a generic Vec<T>, which is a DOS vector.
///
/// The max_allocation() function provides a loose upper bound on the size of the Vec<T: TrustedPreallocate>
/// which can possibly be received from an honest peer. If this limit is too low, Zebra may reject valid messages.
/// In the worst case, setting the lower bound too low could cause Zebra to fall out of consensus by rejecting all messages containing a valid block.
pub trait TrustedPreallocate {
    /// Provides a ***loose upper bound*** on the size of the Vec<T: TrustedPreallocate>
    /// which can possibly be received from an honest peer.
    fn max_allocation() -> u64;
}

/// The length of the longest valid `Vec<u8>` that can be received over the network
///
/// It takes 5 bytes to encode a compactsize representing any number netween 2^16 and (2^32 - 1)
/// MAX_PROTOCOL_MESSAGE_LEN is ~2^21, so the largest Vec<u8> that can be received from an honest peer is
/// (MAX_PROTOCOL_MESSAGE_LEN - 5);
pub(crate) const MAX_U8_ALLOCATION: usize = MAX_PROTOCOL_MESSAGE_LEN - 5;
