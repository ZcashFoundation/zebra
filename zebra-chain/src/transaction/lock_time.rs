use std::io;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::types::BlockHeight;

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
