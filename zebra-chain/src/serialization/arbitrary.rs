use chrono::{TimeZone, Utc, MAX_DATETIME, MIN_DATETIME};
use proptest::{arbitrary::any, prelude::*};

/// Returns a strategy that produces an arbitrary [`chrono::DateTime<Utc>`],
/// based on the full valid range of the type.
///
/// Both the seconds and nanoseconds values are randomised. But leap nanoseconds
/// are never present.
/// https://docs.rs/chrono/0.4.19/chrono/struct.DateTime.html#method.timestamp_subsec_nanos
///
/// Zebra uses these times internally, via [`Utc::now`].
///
/// Some parts of the Zcash network protocol ([`Version`] messages) also use times
/// with an 8-byte seconds value. Unlike this function, they have zero
/// nanoseconds values.
pub fn datetime_full() -> impl Strategy<Value = chrono::DateTime<Utc>> {
    (
        MIN_DATETIME.timestamp()..=MAX_DATETIME.timestamp(),
        0..1_000_000_000_u32,
    )
        .prop_map(|(secs, nsecs)| Utc.timestamp(secs, nsecs))
}

/// Returns a strategy that produces an arbitrary time from a [`u32`] number
/// of seconds past the epoch.
///
/// The nanoseconds value is always zero.
///
/// The Zcash protocol typically uses 4-byte seconds values, except for the
/// [`Version`] message.
pub fn datetime_u32() -> impl Strategy<Value = chrono::DateTime<Utc>> {
    any::<u32>().prop_map(|secs| Utc.timestamp(secs.into(), 0))
}
