//! Arbitrary data generation for serialization proptests

use std::convert::TryInto;

use chrono::{DateTime, TimeZone, Utc};
use proptest::{arbitrary::any, prelude::*};

use super::{
    CompactSizeMessage, DateTime32, TrustedPreallocate, ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN,
};

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
/// Some parts of the Zcash network protocol (`Version` messages) also use times
/// with an 8-byte seconds value. Unlike this function, they have zero
/// nanoseconds values. (So they never have `chrono` leap seconds.)
pub fn datetime_full() -> impl Strategy<Value = chrono::DateTime<Utc>> {
    (
        // TODO: should we be subtracting 1 from the maximum timestamp?
        DateTime::<Utc>::MIN_UTC.timestamp()..=DateTime::<Utc>::MAX_UTC.timestamp(),
        // > The nanosecond part can exceed 1,000,000,000 in order to represent a leap second,
        // > but only when secs % 60 == 59. (The true "UNIX timestamp" cannot represent a leap second
        // > unambiguously.)
        //
        // https://docs.rs/chrono/latest/chrono/offset/trait.TimeZone.html#method.timestamp_opt
        //
        // We use `1_000_000_000` as the right side of the range to avoid that issue.
        0..1_000_000_000_u32,
    )
        .prop_map(|(secs, nsecs)| {
            Utc.timestamp_opt(secs, nsecs)
                .single()
                .expect("in-range number of seconds and valid nanosecond")
        })
}

/// Returns a strategy that produces an arbitrary time from a [`u32`] number
/// of seconds past the epoch.
///
/// The nanoseconds value is always zero.
///
/// The Zcash protocol typically uses 4-byte seconds values, except for the
/// `Version` message.
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

/// Allocate a maximum-sized vector of cloned `item`s, and serialize that array.
///
/// Returns the following calculations on `item`:
///   smallest_disallowed_vec_len
///   smallest_disallowed_serialized_len
///   largest_allowed_vec_len
///   largest_allowed_serialized_len
///
/// For varible-size types, `largest_allowed_serialized_len` might not fit within
/// `MAX_PROTOCOL_MESSAGE_LEN` or `MAX_BLOCK_SIZE`.
pub fn max_allocation_is_big_enough<T>(item: T) -> (usize, usize, usize, usize)
where
    T: TrustedPreallocate + ZcashSerialize + Clone,
{
    let max_allocation: usize = T::max_allocation().try_into().unwrap();
    let mut smallest_disallowed_vec = vec![item; max_allocation + 1];

    let smallest_disallowed_serialized = smallest_disallowed_vec
        .zcash_serialize_to_vec()
        .expect("Serialization to vec must succeed");
    let smallest_disallowed_vec_len = smallest_disallowed_vec.len();

    // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
    smallest_disallowed_vec.pop();
    let largest_allowed_vec = smallest_disallowed_vec;
    let largest_allowed_serialized = largest_allowed_vec
        .zcash_serialize_to_vec()
        .expect("Serialization to vec must succeed");

    (
        smallest_disallowed_vec_len,
        smallest_disallowed_serialized.len(),
        largest_allowed_vec.len(),
        largest_allowed_serialized.len(),
    )
}
