//! Arbitrary data generation for serialization proptests

use std::convert::TryInto;

use chrono::{TimeZone, Utc, MAX_DATETIME, MIN_DATETIME};
use proptest::{arbitrary::any, prelude::*};

use super::{CompactSizeMessage, DateTime32, MAX_PROTOCOL_MESSAGE_LEN};

impl Arbitrary for DateTime32 {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        any::<u32>().prop_map(Into::into).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// Returns a strategy that produces an arbitrary [`chrono::DateTime<Utc>`],
/// based on the full valid range of the type.
///
/// Both the seconds and nanoseconds values are randomised, including leap seconds:
/// <https://docs.rs/chrono/0.4.19/chrono/naive/struct.NaiveTime.html#leap-second-handling>
///
/// Wherever possible, Zebra should handle leap seconds by:
/// - making durations and intervals 3 seconds or longer,
/// - avoiding complex time-based calculations, and
/// - avoiding relying on subsecond precision or time order.
/// When monotonic times are needed, use the opaque `std::time::Instant` type.
///
/// # Usage
///
/// Zebra uses these times internally, typically via [`Utc::now`].
///
/// Some parts of the Zcash network protocol ([`Version`] messages) also use times
/// with an 8-byte seconds value. Unlike this function, they have zero
/// nanoseconds values. (So they never have `chrono` leap seconds.)
pub fn datetime_full() -> impl Strategy<Value = chrono::DateTime<Utc>> {
    (
        // TODO: should we be subtracting 1 from the maximum timestamp?
        MIN_DATETIME.timestamp()..=MAX_DATETIME.timestamp(),
        0..2_000_000_000_u32,
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
///
/// TODO: replace this strategy with `any::<DateTime32>()`.
pub fn datetime_u32() -> impl Strategy<Value = chrono::DateTime<Utc>> {
    any::<DateTime32>().prop_map(Into::into)
}

impl Arbitrary for CompactSizeMessage {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (0..=MAX_PROTOCOL_MESSAGE_LEN)
            .prop_map(|size| {
                size.try_into()
                    .expect("MAX_PROTOCOL_MESSAGE_LEN fits in CompactSizeMessage")
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
