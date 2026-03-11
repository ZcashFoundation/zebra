//! Converting Zcash consensus-critical data structures into bytes.

use std::{io, net::Ipv6Addr};

use super::{AtLeastOne, CompactSizeMessage};

/// The maximum length of a Zcash message, in bytes.
///
/// This value is used to calculate safe preallocation limits for some types
pub const MAX_PROTOCOL_MESSAGE_LEN: usize = 2 * 1024 * 1024;

/// Consensus-critical serialization for Zcash.
///
/// This trait provides a generic serialization for consensus-critical
/// formats, such as network messages, transactions, blocks, etc.
///
/// It is intended for use only for consensus-critical formats.
/// Internal serialization can freely use `serde`, or any other format.
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

    /// Get the size of `self` by using a fake writer.
    fn zcash_serialized_size(&self) -> usize {
        let mut writer = FakeWriter(0);
        self.zcash_serialize(&mut writer)
            .expect("writer should never fail");
        writer.0
    }
}

/// A fake writer helper used to get object lengths without allocating RAM.
pub struct FakeWriter(pub usize);

impl std::io::Write for FakeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();

        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serialize a `Vec` as a CompactSize number of items, then the items. This is
/// the most common format in Zcash.
///
/// See `zcash_serialize_external_count` for more details, and usage information.
impl<T: ZcashSerialize> ZcashSerialize for Vec<T> {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let len: CompactSizeMessage = self
            .len()
            .try_into()
            .expect("len fits in MAX_PROTOCOL_MESSAGE_LEN");
        len.zcash_serialize(&mut writer)?;

        zcash_serialize_external_count(self, writer)
    }
}

/// Serialize an `AtLeastOne` vector as a CompactSize number of items, then the
/// items. This is the most common format in Zcash.
impl<T: ZcashSerialize> ZcashSerialize for AtLeastOne<T> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.as_vec().zcash_serialize(&mut writer)
    }
}

/// Serialize a byte vector as a CompactSize number of items, then the items.
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
    CompactSizeMessage::try_from(vec.len())?.zcash_serialize(&mut writer)?;

    zcash_serialize_bytes_external_count(vec, writer)
}

/// Serialize an empty list of items, by writing a zero CompactSize length.
/// (And no items.)
pub fn zcash_serialize_empty_list<W: io::Write>(writer: W) -> Result<(), io::Error> {
    CompactSizeMessage::default().zcash_serialize(writer)
}

/// Serialize a typed `Vec` **without** writing the number of items as a
/// CompactSize.
///
/// In Zcash, most arrays are stored as a CompactSize, followed by that number
/// of items of type `T`. But in `Transaction::V5`, some types are serialized as
/// multiple arrays in different locations, with a single CompactSize before the
/// first array.
///
/// ## Usage
///
/// Use `zcash_serialize_external_count` when the array count is determined by
/// other data, or a consensus rule.
///
/// Use `Vec::zcash_serialize` for data that contains CompactSize count,
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
/// CompactSize.
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

// We don't impl ZcashSerialize for Ipv4Addr or SocketAddrs,
// because the IPv4 and port formats are different in addr (v1) and addrv2 messages.

/// Write a Bitcoin-encoded IPv6 address.
impl ZcashSerialize for Ipv6Addr {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_all(&self.octets())
    }
}
