use std::{convert::TryInto, io};

use super::{ReadZcashExt, SerializationError, MAX_PROTOCOL_MESSAGE_LEN};
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

impl<T: ZcashDeserialize + TrustedPreallocate> ZcashDeserialize for Vec<T> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let len = reader.read_compactsize()?;
        if len > T::max_allocation() {
            return Err(SerializationError::Parse(
                "Vector longer than max_allocation",
            ));
        }
        let mut vec = Vec::with_capacity(len.try_into()?);
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
// It takes 5 bytes to encode a compactsize representing any number netween 2^16 and (2^32 - 1)
// MAX_PROTOCOL_MESSAGE_LEN is ~2^21, so the largest Vec<u8> that can be received from an honest peer is
// (MAX_PROTOCOL_MESSAGE_LEN - 5);
const MAX_U8_ALLOCATION: usize = MAX_PROTOCOL_MESSAGE_LEN - 5;

/// Implement ZcashDeserialize for Vec<u8> directly instead of using the blanket Vec implementation
///
/// This allows us to optimize the inner loop into a single call to `read_exact()`
/// We don't implement TrustedPreallocate for u8 to allow this optimization without relying on specialization.
impl ZcashDeserialize for Vec<u8> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let len = reader.read_compactsize()?.try_into()?;
        if len > MAX_U8_ALLOCATION {
            return Err(SerializationError::Parse(
                "Vector longer than max_allocation",
            ));
        }
        let mut vec = vec![0u8; len];
        reader.read_exact(&mut vec)?;
        Ok(vec)
    }
}

#[cfg(test)]
mod test_u8_deserialize {
    use super::MAX_U8_ALLOCATION;
    use crate::serialization::MAX_PROTOCOL_MESSAGE_LEN;
    use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
    use proptest::{collection::size_range, prelude::*};
    use std::matches;

    // Allow direct serialization of Vec<u8> for these tests. We don't usuall allow this because some types have
    // specific rules for about serialization of their inner Vec<u8>. This method could be easily misused if it applied
    // more generally.
    impl ZcashSerialize for u8 {
        fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
            writer.write_all(&[*self])
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(3))]
        #[test]
        /// Confirm that deserialize yields the expected result for any vec smaller than `MAX_U8_ALLOCATION`
        fn u8_ser_deser_roundtrip(input in any_with::<Vec<u8>>(size_range(MAX_U8_ALLOCATION).lift()) ) {
            let serialized = input.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            let cursor = std::io::Cursor::new(serialized);
            let deserialized = <Vec<u8>>::zcash_deserialize(cursor).expect("deserialization from vec must succeed");
            prop_assert_eq!(deserialized, input)
        }
    }

    #[test]
    /// Confirm that deserialize allows vectors with length up to and including `MAX_U8_ALLOCATION`
    fn u8_deser_accepts_max_valid_input() {
        let serialized = vec![0u8; MAX_U8_ALLOCATION]
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        let cursor = std::io::Cursor::new(serialized);
        let deserialized = <Vec<u8>>::zcash_deserialize(cursor);
        assert!(deserialized.is_ok())
    }
    #[test]
    /// Confirm that rejects vectors longer than `MAX_U8_ALLOCATION`
    fn u8_deser_throws_when_input_too_large() {
        let serialized = vec![0u8; MAX_U8_ALLOCATION + 1]
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        let cursor = std::io::Cursor::new(serialized);
        let deserialized = <Vec<u8>>::zcash_deserialize(cursor);

        assert!(matches!(
            deserialized,
            Err(SerializationError::Parse(
                "Vector longer than max_allocation"
            ))
        ))
    }

    #[test]
    /// Confirm that every u8 takes exactly 1 byte when serialized.
    /// This verifies that our calculated `MAX_U8_ALLOCATION` is indeed an upper bound.
    fn u8_size_is_correct() {
        for byte in std::u8::MIN..=std::u8::MAX {
            let serialized = byte
                .zcash_serialize_to_vec()
                .expect("Serialization to vec must succeed");
            assert!(serialized.len() == 1)
        }
    }

    #[test]
    /// Verify that...
    /// 1. The smallest disallowed `Vec<u8>` is too big to include in a Zcash Wire Protocol message
    /// 2. The largest allowed `Vec<u8>`is exactly the size of a maximal Zcash Wire Protocol message
    fn u8_max_allocation_is_correct() {
        let mut shortest_disallowed_vec = vec![0u8; MAX_U8_ALLOCATION + 1];
        let shortest_disallowed_serialized = shortest_disallowed_vec
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");

        // Confirm that shortest_disallowed_vec is only one item larger than the limit
        assert_eq!((shortest_disallowed_vec.len() - 1), MAX_U8_ALLOCATION);
        // Confirm that shortest_disallowed_vec is too large to be included in a valid zcash message
        assert!(shortest_disallowed_serialized.len() > MAX_PROTOCOL_MESSAGE_LEN);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        shortest_disallowed_vec.pop();
        let longest_allowed_vec = shortest_disallowed_vec;
        let longest_allowed_serialized = longest_allowed_vec
            .zcash_serialize_to_vec()
            .expect("serialization to vec must succed");

        // Check that our largest_allowed_vec contains the maximum number of items
        assert_eq!(longest_allowed_vec.len(), MAX_U8_ALLOCATION);
        // Check that our largest_allowed_vec is the size of a maximal protocol message
        assert_eq!(longest_allowed_serialized.len(), MAX_PROTOCOL_MESSAGE_LEN);
    }
}
