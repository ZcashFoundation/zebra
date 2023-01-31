//! Transaction LockTime.

use std::io;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};

use crate::{
    block::{self, Height},
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
};

/// A Bitcoin-style `locktime`, representing either a block height or an epoch
/// time.
///
/// # Invariants
///
/// Users should not construct a [`LockTime`] with:
///   - a [`block::Height`] greater than [`LockTime::MAX_HEIGHT`],
///   - a timestamp before 6 November 1985
///     (Unix timestamp less than [`LockTime::MIN_TIMESTAMP`]), or
///   - a timestamp after 5 February 2106
///     (Unix timestamp greater than [`LockTime::MAX_TIMESTAMP`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LockTime {
    /// The transaction can only be included in a block if the block height is
    /// strictly greater than this height
    Height(block::Height),
    /// The transaction can only be included in a block if the block time is
    /// strictly greater than this timestamp
    Time(DateTime<Utc>),
}

impl LockTime {
    /// The minimum [`LockTime::Time`], as a Unix timestamp in seconds.
    ///
    /// Users should not construct [`LockTime`]s with [`LockTime::Time`] lower
    /// than [`LockTime::MIN_TIMESTAMP`].
    ///
    /// If a [`LockTime`] is supposed to be lower than
    /// [`LockTime::MIN_TIMESTAMP`], then a particular [`LockTime::Height`]
    /// applies instead, as described in the spec.
    pub const MIN_TIMESTAMP: i64 = 500_000_000;

    /// The maximum [`LockTime::Time`], as a timestamp in seconds.
    ///
    /// Users should not construct lock times with timestamps greater than
    /// [`LockTime::MAX_TIMESTAMP`]. LockTime is [`u32`] in the spec, so times
    /// are limited to [`u32::MAX`].
    pub const MAX_TIMESTAMP: i64 = u32::MAX as i64;

    /// The maximum [`LockTime::Height`], as a block height.
    ///
    /// Users should not construct lock times with a block height greater than
    /// [`LockTime::MAX_TIMESTAMP`].
    ///
    /// If a [`LockTime`] is supposed to be greater than
    /// [`LockTime::MAX_HEIGHT`], then a particular [`LockTime::Time`] applies
    /// instead, as described in the spec.
    pub const MAX_HEIGHT: Height = Height((Self::MIN_TIMESTAMP - 1) as u32);

    /// Returns a [`LockTime`] that is always unlocked.
    ///
    /// The lock time is set to the block height of the genesis block.
    pub fn unlocked() -> Self {
        LockTime::Height(block::Height(0))
    }

    /// Returns the minimum [`LockTime::Time`], as a [`LockTime`].
    ///
    /// Users should not construct lock times with timestamps lower than the
    /// value returned by this function.
    //
    // TODO: replace Utc.timestamp with DateTime32 (#2211)
    pub fn min_lock_time_timestamp() -> LockTime {
        LockTime::Time(
            Utc.timestamp_opt(Self::MIN_TIMESTAMP, 0)
                .single()
                .expect("in-range number of seconds and valid nanosecond"),
        )
    }

    /// Returns the maximum [`LockTime::Time`], as a [`LockTime`].
    ///
    /// Users should not construct lock times with timestamps greater than the
    /// value returned by this function.
    //
    // TODO: replace Utc.timestamp with DateTime32 (#2211)
    pub fn max_lock_time_timestamp() -> LockTime {
        LockTime::Time(
            Utc.timestamp_opt(Self::MAX_TIMESTAMP, 0)
                .single()
                .expect("in-range number of seconds and valid nanosecond"),
        )
    }

    /// Returns `true` if this lock time is a [`LockTime::Time`], or `false` if it is a [`LockTime::Height`].
    pub fn is_time(&self) -> bool {
        matches!(self, LockTime::Time(_))
    }
}

impl ZcashSerialize for LockTime {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // This implementation does not check the invariants on `LockTime` so that the
        // serialization is fallible only if the underlying writer is. This ensures that
        // we can always compute a hash of a transaction object.
        match self {
            LockTime::Height(block::Height(n)) => writer.write_u32::<LittleEndian>(*n)?,
            LockTime::Time(t) => writer
                .write_u32::<LittleEndian>(t.timestamp().try_into().expect("time is in range"))?,
        }
        Ok(())
    }
}

impl ZcashDeserialize for LockTime {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let n = reader.read_u32::<LittleEndian>()?;
        if n < Self::MIN_TIMESTAMP.try_into().expect("fits in u32") {
            Ok(LockTime::Height(block::Height(n)))
        } else {
            // This can't panic, because all u32 values are valid `Utc.timestamp`s.
            Ok(LockTime::Time(
                Utc.timestamp_opt(n.into(), 0)
                    .single()
                    .expect("in-range number of seconds and valid nanosecond"),
            ))
        }
    }
}
