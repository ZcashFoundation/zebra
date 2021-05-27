//! DateTime types with specific serialization invariants.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{TimeZone, Utc};

use std::{
    convert::{TryFrom, TryInto},
    fmt,
    num::TryFromIntError,
};

use super::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// A date and time, represented by a 32-bit number of seconds since the UNIX epoch.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DateTime32 {
    timestamp: u32,
}

impl DateTime32 {
    /// Returns the number of seconds since the UNIX epoch.
    pub fn timestamp(&self) -> u32 {
        self.timestamp
    }

    /// Returns the equivalent [`chrono::DateTime`].
    pub fn as_chrono(&self) -> chrono::DateTime<Utc> {
        self.into()
    }
}

impl fmt::Debug for DateTime32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DateTime32")
            .field("timestamp", &self.timestamp)
            .field("calendar", &chrono::DateTime::<Utc>::from(*self))
            .finish()
    }
}

impl From<u32> for DateTime32 {
    fn from(value: u32) -> Self {
        DateTime32 { timestamp: value }
    }
}

impl From<&u32> for DateTime32 {
    fn from(value: &u32) -> Self {
        (*value).into()
    }
}

impl From<DateTime32> for chrono::DateTime<Utc> {
    fn from(value: DateTime32) -> Self {
        // chrono::DateTime is guaranteed to hold 32-bit values
        Utc.timestamp(value.timestamp.into(), 0)
    }
}

impl From<&DateTime32> for chrono::DateTime<Utc> {
    fn from(value: &DateTime32) -> Self {
        (*value).into()
    }
}

impl TryFrom<chrono::DateTime<Utc>> for DateTime32 {
    type Error = TryFromIntError;

    /// Convert from a [`chrono::DateTime`] to a [`DateTime32`], discarding any nanoseconds.
    ///
    /// Conversion fails if the number of seconds is outside the `u32` range.
    fn try_from(value: chrono::DateTime<Utc>) -> Result<Self, Self::Error> {
        Ok(Self {
            timestamp: value.timestamp().try_into()?,
        })
    }
}

impl TryFrom<&chrono::DateTime<Utc>> for DateTime32 {
    type Error = TryFromIntError;

    /// Convert from a [`chrono::DateTime`] to a [`DateTime32`], discarding any nanoseconds.
    ///
    /// Conversion fails if the number of seconds is outside the `u32` range.
    fn try_from(value: &chrono::DateTime<Utc>) -> Result<Self, Self::Error> {
        (*value).try_into()
    }
}

impl ZcashSerialize for DateTime32 {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u32::<LittleEndian>(self.timestamp)
    }
}

impl ZcashDeserialize for DateTime32 {
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(DateTime32 {
            timestamp: reader.read_u32::<LittleEndian>()?,
        })
    }
}
