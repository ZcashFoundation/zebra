//! Newtype wrappers for primitive data types with semantic meaning.
#![allow(clippy::unit_arg)]
use crate::serialization::{
    ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};
use std::{
    fmt,
    io::{self, Read},
};

/// A u32 which represents a block height value.
///
/// # Invariants
///
/// Users should not construct block heights greater than `BlockHeight::MAX`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlockHeight(pub u32);

impl std::str::FromStr for BlockHeight {
    type Err = SerializationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse() {
            Ok(h) if (BlockHeight(h) <= BlockHeight::MAX) => Ok(BlockHeight(h)),
            Ok(_) => Err(SerializationError::Parse(
                "BlockHeight exceeds maximum height",
            )),
            Err(_) => Err(SerializationError::Parse(
                "BlockHeight(u32) integer parse error",
            )),
        }
    }
}

#[cfg(test)]
impl Arbitrary for BlockHeight {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (BlockHeight::MIN.0..=BlockHeight::MAX.0)
            .prop_map(BlockHeight)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A Bitcoin-style `locktime`, representing either a block height or an epoch
/// time.
///
/// # Invariants
///
/// Users should not construct a `LockTime` with:
///   - a `BlockHeight` greater than MAX_BLOCK_HEIGHT,
///   - a timestamp before 6 November 1985
///     (Unix timestamp less than MIN_LOCK_TIMESTAMP), or
///   - a timestamp after 5 February 2106
///     (Unix timestamp greater than MAX_LOCK_TIMESTAMP).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LockTime {
    /// Unlock at a particular block height.
    Height(BlockHeight),
    /// Unlock at a particular time.
    Time(DateTime<Utc>),
}

impl BlockHeight {
    /// The minimum BlockHeight.
    ///
    /// Due to the underlying type, it is impossible to construct block heights
    /// less than `BlockHeight::MIN`.
    ///
    /// Style note: Sometimes, `BlockHeight::MIN` is less readable than
    /// `BlockHeight(0)`. Use whichever makes sense in context.
    pub const MIN: BlockHeight = BlockHeight(0);

    /// The maximum BlockHeight.
    ///
    /// Users should not construct block heights greater than `BlockHeight::MAX`.
    pub const MAX: BlockHeight = BlockHeight(499_999_999);

    /// The maximum BlockHeight as a u32, for range patterns.
    ///
    /// `BlockHeight::MAX.0` can't be used in match range patterns, use this
    /// alias instead.
    pub const MAX_AS_U32: u32 = Self::MAX.0;
}

impl LockTime {
    /// The minimum LockTime::Time, as a timestamp in seconds.
    ///
    /// Users should not construct lock times less than `MIN_TIMESTAMP`.
    pub const MIN_TIMESTAMP: i64 = 500_000_000;

    /// The maximum LockTime::Time, as a timestamp in seconds.
    ///
    /// Users should not construct lock times greater than `MAX_TIMESTAMP`.
    /// LockTime is u32 in the spec, so times are limited to u32::MAX.
    pub const MAX_TIMESTAMP: i64 = u32::MAX as i64;

    /// Returns the minimum LockTime::Time, as a LockTime.
    ///
    /// Users should not construct lock times less than `min_lock_timestamp`.
    //
    // When `Utc.timestamp` stabilises as a const function, we can make this a
    // const function.
    pub fn min_lock_time() -> LockTime {
        LockTime::Time(Utc.timestamp(Self::MIN_TIMESTAMP, 0))
    }

    /// Returns the maximum LockTime::Time, as a LockTime.
    ///
    /// Users should not construct lock times greater than `max_lock_timestamp`.
    //
    // When `Utc.timestamp` stabilises as a const function, we can make this a
    // const function.
    pub fn max_lock_time() -> LockTime {
        LockTime::Time(Utc.timestamp(Self::MAX_TIMESTAMP, 0))
    }
}

impl ZcashSerialize for LockTime {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // This implementation does not check the invariants on `LockTime` so that the
        // serialization is fallible only if the underlying writer is. This ensures that
        // we can always compute a hash of a transaction object.
        use LockTime::*;
        match self {
            Height(BlockHeight(n)) => writer.write_u32::<LittleEndian>(*n)?,
            Time(t) => writer.write_u32::<LittleEndian>(t.timestamp() as u32)?,
        }
        Ok(())
    }
}

impl ZcashDeserialize for LockTime {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let n = reader.read_u32::<LittleEndian>()?;
        if n <= BlockHeight::MAX.0 {
            Ok(LockTime::Height(BlockHeight(n)))
        } else {
            Ok(LockTime::Time(Utc.timestamp(n as i64, 0)))
        }
    }
}

#[cfg(test)]
impl Arbitrary for LockTime {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        prop_oneof![
            (BlockHeight::MIN.0..=BlockHeight::MAX.0)
                .prop_map(|n| LockTime::Height(BlockHeight(n))),
            (LockTime::MIN_TIMESTAMP..=LockTime::MAX_TIMESTAMP)
                .prop_map(|n| { LockTime::Time(Utc.timestamp(n as i64, 0)) })
        ]
        .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A sequence of message authentication tags ...
///
/// binding h_sig to each a_sk of the JoinSplit description, computed as
/// described in § 4.10 ‘Non-malleability (Sprout)’ on p. 37
#[derive(PartialEq, Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct MAC([u8; 32]);

impl ZcashDeserialize for MAC {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let bytes = reader.read_32_bytes()?;

        Ok(Self(bytes))
    }
}

impl ZcashSerialize for MAC {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])
    }
}
/// An encoding of a Bitcoin script.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Script(pub Vec<u8>);

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Script")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl ZcashSerialize for Script {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_compactsize(self.0.len() as u64)?;
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for Script {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // XXX what is the max length of a script?
        let len = reader.read_compactsize()?;
        let mut bytes = Vec::new();
        reader.take(len).read_to_end(&mut bytes)?;
        Ok(Script(bytes))
    }
}

#[cfg(test)]
use proptest::prelude::*;

#[cfg(test)]
mod proptests {
    use std::io::Cursor;

    use proptest::prelude::*;

    use super::{LockTime, Script};
    use crate::serialization::{ZcashDeserialize, ZcashSerialize};

    proptest! {
        #[test]
        fn locktime_roundtrip(locktime in any::<LockTime>()) {
            let mut bytes = Cursor::new(Vec::new());
            locktime.zcash_serialize(&mut bytes)?;

            bytes.set_position(0);
            let other_locktime = LockTime::zcash_deserialize(&mut bytes)?;

            prop_assert_eq![locktime, other_locktime];
        }

        #[test]
        fn script_roundtrip(script in any::<Script>()) {
            let mut bytes = Cursor::new(Vec::new());
            script.zcash_serialize(&mut bytes)?;

            bytes.set_position(0);
            let other_script = Script::zcash_deserialize(&mut bytes)?;

            prop_assert_eq![script, other_script];
        }
    }
}
