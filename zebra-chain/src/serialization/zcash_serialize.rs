use std::io;

use super::{AtLeastOne, WriteZcashExt};

/// Consensus-critical serialization for Zcash.
///
/// This trait provides a generic serialization for consensus-critical
/// formats, such as network messages, transactions, blocks, etc. It is intended
/// for use only in consensus-critical contexts; in other contexts, such as
/// internal storage, it would be preferable to use Serde.
pub trait ZcashSerialize: Sized {
    /// Write `self` to the given `writer` using the canonical format.
    ///
    /// This function has a `zcash_` prefix to alert the reader that the
    /// serialization in use is consensus-critical serialization, rather than
    /// some other kind of serialization.
    ///
    /// Notice that the error type is [`std::io::Error`]; this indicates that
    /// serialization MUST be infallible up to errors in the underlying writer.
    /// In other words, any type implementing `ZcashSerialize` must make illegal
    /// states unrepresentable.
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error>;

    /// Helper function to construct a vec to serialize the current struct into
    fn zcash_serialize_to_vec(&self) -> Result<Vec<u8>, io::Error> {
        let mut data = Vec::new();
        self.zcash_serialize(&mut data)?;
        Ok(data)
    }
}

/// Serialize a `Vec` as a compactsize number of items, then the items. This is
/// the most common format in Zcash.
///
/// See `zcash_serialize_external_count` for more details, and usage information.
impl<T: ZcashSerialize> ZcashSerialize for Vec<T> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_compactsize(self.len() as u64)?;
        zcash_serialize_external_count(self, writer)
    }
}

/// Serialize a byte vector as a compactsize number of items, then the items.
///
/// # Correctness
///
/// Most Zcash types have specific rules about serialization of `Vec<u8>`s.
/// Check the spec and consensus rules before using this function.
///
/// See `zcash_serialize_bytes_external_count` for more details, and usage information.
//
// we specifically want to serialize `Vec`s here, rather than generic slices
#[allow(clippy::ptr_arg)]
pub fn zcash_serialize_bytes<W: io::Write>(vec: &Vec<u8>, mut writer: W) -> Result<(), io::Error> {
    writer.write_compactsize(vec.len() as u64)?;
    zcash_serialize_bytes_external_count(vec, writer)
}

/// Serialize an `AtLeastOne` vector as a compactsize number of items, then the
/// items. This is the most common format in Zcash.
impl<T: ZcashSerialize> ZcashSerialize for AtLeastOne<T> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.as_vec().zcash_serialize(&mut writer)
    }
}

/// Serialize a typed `Vec` **without** writing the number of items as a
/// compactsize.
///
/// In Zcash, most arrays are stored as a compactsize, followed by that number
/// of items of type `T`. But in `Transaction::V5`, some types are serialized as
/// multiple arrays in different locations, with a single compactsize before the
/// first array.
///
/// ## Usage
///
/// Use `zcash_serialize_external_count` when the array count is determined by
/// other data, or a consensus rule.
///
/// Use `Vec::zcash_serialize` for data that contains compactsize count,
/// followed by the data array.
///
/// For example, when a single count applies to multiple arrays:
/// 1. Use `Vec::zcash_serialize` for the array that has a data count.
/// 2. Use `zcash_serialize_external_count` for the arrays with no count in the
///    data, passing the length of the first array.
///
/// This function has a `zcash_` prefix to alert the reader that the
/// serialization in use is consensus-critical serialization, rather than
/// some other kind of serialization.
//
// we specifically want to serialize `Vec`s here, rather than generic slices
#[allow(clippy::ptr_arg)]
pub fn zcash_serialize_external_count<W: io::Write, T: ZcashSerialize>(
    vec: &Vec<T>,
    mut writer: W,
) -> Result<(), io::Error> {
    for x in vec {
        x.zcash_serialize(&mut writer)?;
    }
    Ok(())
}

/// Serialize a raw byte `Vec` **without** writing the number of items as a
/// compactsize.
///
/// This is a convenience alias for `writer.write_all(&vec)`.
//
// we specifically want to serialize `Vec`s here, rather than generic slices
#[allow(clippy::ptr_arg)]
pub fn zcash_serialize_bytes_external_count<W: io::Write>(
    vec: &Vec<u8>,
    mut writer: W,
) -> Result<(), io::Error> {
    writer.write_all(vec)
}

/// Write a Bitcoin-encoded UTF-8 `&str`.
impl ZcashSerialize for &str {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        let str_bytes = self.as_bytes().to_vec();
        zcash_serialize_bytes(&str_bytes, writer)
    }
}

/// Write a Bitcoin-encoded UTF-8 `String`.
impl ZcashSerialize for String {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.as_str().zcash_serialize(&mut writer)
    }
}

/// The maximum length of a Zcash message, in bytes.
///
/// This value is used to calculate safe preallocation limits for some types
pub const MAX_PROTOCOL_MESSAGE_LEN: usize = 2 * 1024 * 1024;
