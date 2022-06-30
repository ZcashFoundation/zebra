//! DateTime types with specific serialization invariants.

use std::{
    convert::{TryFrom, TryInto},
    fmt,
    num::TryFromIntError,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{TimeZone, Utc};

use super::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// A date and time, represented by a 32-bit number of seconds since the UNIX epoch.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DateTime32 {
    timestamp: u32,
}

/// An unsigned time duration, represented by a 32-bit number of seconds.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Duration32 {
    seconds: u32,
}

impl DateTime32 {
    /// The earliest possible `DateTime32` value.
    pub const MIN: DateTime32 = DateTime32 {
        timestamp: u32::MIN,
    };

    /// The latest possible `DateTime32` value.
    pub const MAX: DateTime32 = DateTime32 {
        timestamp: u32::MAX,
    };

    /// Returns the number of seconds since the UNIX epoch.
    pub fn timestamp(&self) -> u32 {
        self.timestamp
    }

    /// Returns the equivalent [`chrono::DateTime`].
    pub fn to_chrono(self) -> chrono::DateTime<Utc> {
        self.into()
    }

    /// Returns the current time.
    ///
    /// # Panics
    ///
    /// If the number of seconds since the UNIX epoch is greater than `u32::MAX`.
    pub fn now() -> DateTime32 {
        chrono::Utc::now()
            .try_into()
            .expect("unexpected out of range chrono::DateTime")
    }

    /// Returns the duration elapsed between `earlier` and this time,
    /// or `None` if `earlier` is later than this time.
    pub fn checked_duration_since(&self, earlier: DateTime32) -> Option<Duration32> {
        self.timestamp
            .checked_sub(earlier.timestamp)
            .map(Duration32::from)
    }

    /// Returns duration elapsed between `earlier` and this time,
    /// or zero if `earlier` is later than this time.
    pub fn saturating_duration_since(&self, earlier: DateTime32) -> Duration32 {
        Duration32::from(self.timestamp.saturating_sub(earlier.timestamp))
    }

    /// Returns the duration elapsed since this time,
    /// or if this time is in the future, returns `None`.
    #[allow(clippy::unwrap_in_result)]
    pub fn checked_elapsed(&self, now: chrono::DateTime<Utc>) -> Option<Duration32> {
        DateTime32::try_from(now)
            .expect("unexpected out of range chrono::DateTime")
            .checked_duration_since(*self)
    }

    /// Returns the duration elapsed since this time,
    /// or if this time is in the future, returns zero.
    pub fn saturating_elapsed(&self, now: chrono::DateTime<Utc>) -> Duration32 {
        DateTime32::try_from(now)
            .expect("unexpected out of range chrono::DateTime")
            .saturating_duration_since(*self)
    }

    /// Returns the time that is `duration` after this time.
    /// If the calculation overflows, returns `None`.
    pub fn checked_add(&self, duration: Duration32) -> Option<DateTime32> {
        self.timestamp
            .checked_add(duration.seconds)
            .map(DateTime32::from)
    }

    /// Returns the time that is `duration` after this time.
    /// If the calculation overflows, returns `DateTime32::MAX`.
    pub fn saturating_add(&self, duration: Duration32) -> DateTime32 {
        DateTime32::from(self.timestamp.saturating_add(duration.seconds))
    }

    /// Returns the time that is `duration` before this time.
    /// If the calculation underflows, returns `None`.
    pub fn checked_sub(&self, duration: Duration32) -> Option<DateTime32> {
        self.timestamp
            .checked_sub(duration.seconds)
            .map(DateTime32::from)
    }

    /// Returns the time that is `duration` before this time.
    /// If the calculation underflows, returns `DateTime32::MIN`.
    pub fn saturating_sub(&self, duration: Duration32) -> DateTime32 {
        DateTime32::from(self.timestamp.saturating_sub(duration.seconds))
    }
}

impl Duration32 {
    /// The earliest possible `Duration32` value.
    pub const MIN: Duration32 = Duration32 { seconds: u32::MIN };

    /// The latest possible `Duration32` value.
    pub const MAX: Duration32 = Duration32 { seconds: u32::MAX };

    /// Creates a new [`Duration32`] to represent the given amount of seconds.
    pub const fn from_seconds(seconds: u32) -> Self {
        Duration32 { seconds }
    }

    /// Creates a new [`Duration32`] to represent the given amount of minutes.
    ///
    /// If the resulting number of seconds does not fit in a [`u32`], [`Duration32::MAX`] is
    /// returned.
    pub const fn from_minutes(minutes: u32) -> Self {
        Duration32::from_seconds(minutes.saturating_mul(60))
    }

    /// Creates a new [`Duration32`] to represent the given amount of hours.
    ///
    /// If the resulting number of seconds does not fit in a [`u32`], [`Duration32::MAX`] is
    /// returned.
    pub const fn from_hours(hours: u32) -> Self {
        Duration32::from_minutes(hours.saturating_mul(60))
    }

    /// Creates a new [`Duration32`] to represent the given amount of days.
    ///
    /// If the resulting number of seconds does not fit in a [`u32`], [`Duration32::MAX`] is
    /// returned.
    pub const fn from_days(days: u32) -> Self {
        Duration32::from_hours(days.saturating_mul(24))
    }

    /// Returns the number of seconds in this duration.
    pub fn seconds(&self) -> u32 {
        self.seconds
    }

    /// Returns the equivalent [`chrono::Duration`].
    pub fn to_chrono(self) -> chrono::Duration {
        self.into()
    }

    /// Returns the equivalent [`std::time::Duration`].
    pub fn to_std(self) -> std::time::Duration {
        self.into()
    }

    /// Returns a duration that is `duration` longer than this duration.
    /// If the calculation overflows, returns `None`.
    pub fn checked_add(&self, duration: Duration32) -> Option<Duration32> {
        self.seconds
            .checked_add(duration.seconds)
            .map(Duration32::from)
    }

    /// Returns a duration that is `duration` longer than this duration.
    /// If the calculation overflows, returns `Duration32::MAX`.
    pub fn saturating_add(&self, duration: Duration32) -> Duration32 {
        Duration32::from(self.seconds.saturating_add(duration.seconds))
    }

    /// Returns a duration that is `duration` shorter than this duration.
    /// If the calculation underflows, returns `None`.
    pub fn checked_sub(&self, duration: Duration32) -> Option<Duration32> {
        self.seconds
            .checked_sub(duration.seconds)
            .map(Duration32::from)
    }

    /// Returns a duration that is `duration` shorter than this duration.
    /// If the calculation underflows, returns `Duration32::MIN`.
    pub fn saturating_sub(&self, duration: Duration32) -> Duration32 {
        Duration32::from(self.seconds.saturating_sub(duration.seconds))
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

impl fmt::Debug for Duration32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Duration32")
            .field("seconds", &self.seconds)
            .field("calendar", &chrono::Duration::from(*self))
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

impl From<u32> for Duration32 {
    fn from(value: u32) -> Self {
        Duration32 { seconds: value }
    }
}

impl From<&u32> for Duration32 {
    fn from(value: &u32) -> Self {
        (*value).into()
    }
}

impl From<Duration32> for chrono::Duration {
    fn from(value: Duration32) -> Self {
        // chrono::Duration is guaranteed to hold 32-bit values
        chrono::Duration::seconds(value.seconds.into())
    }
}

impl From<&Duration32> for chrono::Duration {
    fn from(value: &Duration32) -> Self {
        (*value).into()
    }
}

impl From<Duration32> for std::time::Duration {
    fn from(value: Duration32) -> Self {
        // std::time::Duration is guaranteed to hold 32-bit values
        std::time::Duration::from_secs(value.seconds.into())
    }
}

impl From<&Duration32> for std::time::Duration {
    fn from(value: &Duration32) -> Self {
        (*value).into()
    }
}

impl TryFrom<chrono::DateTime<Utc>> for DateTime32 {
    type Error = TryFromIntError;

    /// Convert from a [`chrono::DateTime`] to a [`DateTime32`], discarding any nanoseconds.
    ///
    /// Conversion fails if the number of seconds since the UNIX epoch is outside the `u32` range.
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
    /// Conversion fails if the number of seconds since the UNIX epoch is outside the `u32` range.
    fn try_from(value: &chrono::DateTime<Utc>) -> Result<Self, Self::Error> {
        (*value).try_into()
    }
}

impl TryFrom<chrono::Duration> for Duration32 {
    type Error = TryFromIntError;

    /// Convert from a [`chrono::Duration`] to a [`Duration32`], discarding any nanoseconds.
    ///
    /// Conversion fails if the number of seconds since the UNIX epoch is outside the `u32` range.
    fn try_from(value: chrono::Duration) -> Result<Self, Self::Error> {
        Ok(Self {
            seconds: value.num_seconds().try_into()?,
        })
    }
}

impl TryFrom<&chrono::Duration> for Duration32 {
    type Error = TryFromIntError;

    /// Convert from a [`chrono::Duration`] to a [`Duration32`], discarding any nanoseconds.
    ///
    /// Conversion fails if the number of seconds in the duration is outside the `u32` range.
    fn try_from(value: &chrono::Duration) -> Result<Self, Self::Error> {
        (*value).try_into()
    }
}

impl TryFrom<std::time::Duration> for Duration32 {
    type Error = TryFromIntError;

    /// Convert from a [`std::time::Duration`] to a [`Duration32`], discarding any nanoseconds.
    ///
    /// Conversion fails if the number of seconds in the duration is outside the `u32` range.
    fn try_from(value: std::time::Duration) -> Result<Self, Self::Error> {
        Ok(Self {
            seconds: value.as_secs().try_into()?,
        })
    }
}

impl TryFrom<&std::time::Duration> for Duration32 {
    type Error = TryFromIntError;

    /// Convert from a [`std::time::Duration`] to a [`Duration32`], discarding any nanoseconds.
    ///
    /// Conversion fails if the number of seconds in the duration is outside the `u32` range.
    fn try_from(value: &std::time::Duration) -> Result<Self, Self::Error> {
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
