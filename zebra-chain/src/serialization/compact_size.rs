//! Types and serialization for the Bitcoin `CompactSize` format.
//!
//! Zebra decodes `CompactSize` fields into two different types,
//! depending on the consensus rules that apply to that type:
//! - [`CompactSizeMessage`] for sizes that must be less than the network message limit, and
//! - [`CompactSize64`] for flags, arbitrary counts, and sizes that span multiple blocks.

use std::convert::{TryFrom, TryInto};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::serialization::{
    SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
    MAX_PROTOCOL_MESSAGE_LEN,
};

/// A compactSize-encoded field that is limited to [`MAX_PROTOCOL_MESSAGE_LEN`].
/// Used for sizes or counts of objects that are sent in network messages.
///
/// # Security
///
/// Deserialized sizes must be validated before being used to allocate memory.
///
/// Preallocating vectors using untrusted `CompactSize`s allows memory
/// denial of service attacks. Valid sizes must be less than
/// `MAX_PROTOCOL_MESSAGE_LEN / min_serialized_item_bytes` (or a lower limit
/// specified by the Zcash consensus rules or Bitcoin network protocol).
///
/// As a defence-in-depth for memory preallocation attacks,
/// Zebra rejects sizes greater than the protocol message length limit.
/// (These sizes should be impossible, because each array items takes at
/// least one byte.)
///
/// # Serialization Examples
///
/// ```
/// use zebra_chain::serialization::{CompactSizeMessage, ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN};
/// use std::convert::TryFrom;
///
/// let size = CompactSizeMessage::try_from(0x12).unwrap();
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\x12");
///
/// let size = CompactSizeMessage::try_from(0xfd).unwrap();
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\xfd\xfd\x00");
///
/// let size = CompactSizeMessage::try_from(0xaafd).unwrap();
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\xfd\xfd\xaa");
///
/// let max_size = u64::try_from(MAX_PROTOCOL_MESSAGE_LEN).unwrap();
/// let size = CompactSizeMessage::try_from(max_size).unwrap();
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\xfe\x00\x00\x20\x00");
/// ```
///
/// [`CompactSizeMessage`]s greater than the maximum network message length
/// can not be constructed:
/// ```
/// # use zebra_chain::serialization::{CompactSizeMessage, MAX_PROTOCOL_MESSAGE_LEN};
/// # use std::convert::TryFrom;
/// # let max_size = u64::try_from(MAX_PROTOCOL_MESSAGE_LEN).unwrap();
/// let err = CompactSizeMessage::try_from(max_size + 1).unwrap_err();
///
/// let err = CompactSizeMessage::try_from(0xbbaafd).unwrap_err();
///
/// let err = CompactSizeMessage::try_from(0x22ccbbaafd).unwrap_err();
/// ```
///
/// # Deserialization Examples
///
/// ```
/// use zebra_chain::serialization::{CompactSizeMessage, ZcashDeserializeInto, MAX_PROTOCOL_MESSAGE_LEN};
/// use std::{convert::TryFrom, io::Cursor};
///
/// assert_eq!(
///     CompactSizeMessage::try_from(0x12).unwrap(),
///     Cursor::new(b"\x12").zcash_deserialize_into().unwrap(),
/// );
///
/// assert_eq!(
///     CompactSizeMessage::try_from(0xfd).unwrap(),
///     Cursor::new(b"\xfd\xfd\x00").zcash_deserialize_into().unwrap(),
/// );
///
/// assert_eq!(
///     CompactSizeMessage::try_from(0xaafd).unwrap(),
///     Cursor::new(b"\xfd\xfd\xaa").zcash_deserialize_into().unwrap(),
/// );
///
/// let max_size = u64::try_from(MAX_PROTOCOL_MESSAGE_LEN).unwrap();
/// assert_eq!(
///     CompactSizeMessage::try_from(max_size).unwrap(),
///     Cursor::new(b"\xfe\x00\x00\x20\x00").zcash_deserialize_into().unwrap(),
/// );
/// ```
///
/// [`CompactSizeMessage`]s greater than the maximum network message length are invalid.
/// They return a [`SerializationError::Parse`]:
/// ```
/// # use zebra_chain::serialization::{CompactSizeMessage, ZcashDeserialize, MAX_PROTOCOL_MESSAGE_LEN};
/// # use std::{convert::TryFrom, io::Cursor};
/// let max_size_plus_one = Cursor::new(b"\xfe\x01\x00\x20\x00");
/// let err = CompactSizeMessage::zcash_deserialize(max_size_plus_one).unwrap_err();
///
/// let bytes = Cursor::new(b"\xfe\xfd\xaa\xbb\x00");
/// let err = CompactSizeMessage::zcash_deserialize(bytes).unwrap_err();
///
/// let bytes = Cursor::new(b"\xff\xfd\xaa\xbb\xcc\x22\x00\x00\x00");
/// let err = CompactSizeMessage::zcash_deserialize(bytes).unwrap_err();
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CompactSizeMessage(
    /// The number of items in a Zcash message.
    ///
    /// This field is private to enforce the [`MAX_PROTOCOL_MESSAGE_LEN`] limit.
    u32,
);

/// An arbitrary compactSize-encoded field.
/// Used for flags, arbitrary counts, and sizes that span multiple blocks.
///
/// # Security
///
/// Deserialized sizes must be validated before being used to allocate memory.
///
/// Most allocation sizes should use [`CompactSizeMessage`],
/// because it is limited to [`MAX_PROTOCOL_MESSAGE_LEN`].
///
/// # Serialization Examples
///
/// ```
/// use zebra_chain::serialization::{CompactSize64, ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN};
/// use std::convert::TryFrom;
///
/// let size = CompactSize64::from(0x12);
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\x12");
///
/// let size = CompactSize64::from(0xfd);
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\xfd\xfd\x00");
///
/// let size = CompactSize64::from(0xaafd);
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\xfd\xfd\xaa");
///
/// let max_size = u64::try_from(MAX_PROTOCOL_MESSAGE_LEN).unwrap();
/// let size = CompactSize64::from(max_size);
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\xfe\x00\x00\x20\x00");
///
/// let size = CompactSize64::from(max_size + 1);
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\xfe\x01\x00\x20\x00");
///
/// let size = CompactSize64::from(0xbbaafd);
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\xfe\xfd\xaa\xbb\x00");
///
/// let size = CompactSize64::from(0x22ccbbaafd);
/// let buf = size.zcash_serialize_to_vec().unwrap();
/// assert_eq!(buf, b"\xff\xfd\xaa\xbb\xcc\x22\x00\x00\x00");
/// ```
///
/// # Deserialization Examples
///
/// ```
/// use zebra_chain::serialization::{CompactSize64, ZcashDeserializeInto, MAX_PROTOCOL_MESSAGE_LEN};
/// use std::{convert::TryFrom, io::Cursor};
///
/// assert_eq!(
///     CompactSize64::from(0x12),
///     Cursor::new(b"\x12").zcash_deserialize_into().unwrap(),
/// );
///
/// assert_eq!(
///     CompactSize64::from(0xfd),
///     Cursor::new(b"\xfd\xfd\x00").zcash_deserialize_into().unwrap(),
/// );
///
/// assert_eq!(
///     CompactSize64::from(0xaafd),
///     Cursor::new(b"\xfd\xfd\xaa").zcash_deserialize_into().unwrap(),
/// );
///
/// let max_size = u64::try_from(MAX_PROTOCOL_MESSAGE_LEN).unwrap();
/// assert_eq!(
///     CompactSize64::from(max_size),
///     Cursor::new(b"\xfe\x00\x00\x20\x00").zcash_deserialize_into().unwrap(),
/// );
///
/// assert_eq!(
///     CompactSize64::from(max_size + 1),
///     Cursor::new(b"\xfe\x01\x00\x20\x00").zcash_deserialize_into().unwrap(),
/// );
///
/// assert_eq!(
///     CompactSize64::from(0xbbaafd),
///     Cursor::new(b"\xfe\xfd\xaa\xbb\x00").zcash_deserialize_into().unwrap(),
/// );
///
/// assert_eq!(
///     CompactSize64::from(0x22ccbbaafd),
///     Cursor::new(b"\xff\xfd\xaa\xbb\xcc\x22\x00\x00\x00").zcash_deserialize_into().unwrap(),
/// );
///```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CompactSize64(
    /// A numeric value.
    ///
    /// This field is private for consistency with [`CompactSizeMessage`].
    u64,
);

// We don't implement conversions between CompactSizeMessage and CompactSize64,
// because we want to distinguish fields with different numeric constraints.
//
// We don't implement From<CompactSizeMessage> for u16 or u8,
// because we want all values to go through the same code paths.
// (And we don't want to accidentally truncate using `as`.)

impl TryFrom<u64> for CompactSizeMessage {
    type Error = SerializationError;

    #[inline]
    fn try_from(size: u64) -> Result<Self, Self::Error> {
        use SerializationError::Parse;

        let size: u32 = size.try_into()?;

        // # Security
        // Defence-in-depth for memory DoS via preallocation.
        if size
            > MAX_PROTOCOL_MESSAGE_LEN
                .try_into()
                .expect("MAX_PROTOCOL_MESSAGE_LEN fits in u32")
        {
            // This could be invalid data from peers, so we return a parse error.
            Err(Parse("compactsize larger than protocol message limit"))?;
        }

        Ok(CompactSizeMessage(size))
    }
}

impl From<CompactSizeMessage> for u64 {
    #[inline]
    fn from(size: CompactSizeMessage) -> Self {
        size.0.into()
    }
}

impl From<CompactSize64> for u64 {
    #[inline]
    fn from(size: CompactSize64) -> Self {
        size.0
    }
}

impl From<u64> for CompactSize64 {
    #[inline]
    fn from(size: u64) -> Self {
        CompactSize64(size)
    }
}

impl ZcashSerialize for CompactSizeMessage {
    /// Serialize a CompactSizeMessage into the Zcash consensus-critical format.
    ///
    /// # Panics
    ///
    /// If the value exceeds `MAX_PROTOCOL_MESSAGE_LEN`.
    #[inline]
    fn zcash_serialize<W: std::io::Write>(&self, writer: W) -> Result<(), std::io::Error> {
        // # Security
        // Defence-in-depth for memory DoS via preallocation.
        if self.0
            > MAX_PROTOCOL_MESSAGE_LEN
                .try_into()
                .expect("usize fits in u64")
        {
            // This is validated data inside Zebra, so we panic.
            panic!("compactsize larger than protocol message limit");
        }

        // Use the same serialization format as CompactSize64.
        let size: u64 = self.0.into();
        CompactSize64(size).zcash_serialize(writer)
    }
}

impl ZcashDeserialize for CompactSizeMessage {
    #[inline]
    fn zcash_deserialize<R: std::io::Read>(reader: R) -> Result<Self, SerializationError> {
        // Use the same deserialization format as CompactSize64.
        let size: CompactSize64 = reader.zcash_deserialize_into()?;

        size.0.try_into()
    }
}

impl ZcashSerialize for CompactSize64 {
    #[inline]
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        let n = self.0;
        match n {
            0x0000_0000..=0x0000_00fc => writer.write_u8(n as u8),
            0x0000_00fd..=0x0000_ffff => {
                writer.write_u8(0xfd)?;
                writer.write_u16::<LittleEndian>(n as u16)
            }
            0x0001_0000..=0xffff_ffff => {
                writer.write_u8(0xfe)?;
                writer.write_u32::<LittleEndian>(n as u32)
            }
            _ => {
                writer.write_u8(0xff)?;
                writer.write_u64::<LittleEndian>(n)
            }
        }
    }
}

impl ZcashDeserialize for CompactSize64 {
    #[inline]
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        use SerializationError::Parse;

        let flag_byte = reader.read_u8()?;
        let size = match flag_byte {
            n @ 0x00..=0xfc => Ok(n as u64),
            0xfd => match reader.read_u16::<LittleEndian>()? {
                n @ 0x0000_00fd..=0x0000_ffff => Ok(n as u64),
                _ => Err(Parse("non-canonical compactsize")),
            },
            0xfe => match reader.read_u32::<LittleEndian>()? {
                n @ 0x0001_0000..=0xffff_ffff => Ok(n as u64),
                _ => Err(Parse("non-canonical compactsize")),
            },
            0xff => match reader.read_u64::<LittleEndian>()? {
                n @ 0x1_0000_0000..=0xffff_ffff_ffff_ffff => Ok(n),
                _ => Err(Parse("non-canonical compactsize")),
            },
        }?;

        Ok(CompactSize64(size))
    }
}
