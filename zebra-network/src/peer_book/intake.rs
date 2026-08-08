//! Validation for gossiped peer addresses entering the peer book.
//!
//! Every gossiped address passes through this module before it reaches the
//! address book, so intake hygiene lives in one place.

use zebra_chain::serialization::DateTime32;

use crate::meta_addr::MetaAddr;

/// Check new `addrs` before adding them to the address book.
///
/// `last_seen_limit` is the maximum permitted last seen time, typically
/// [`Utc::now`](chrono::Utc::now).
///
/// If the data in an address is invalid, this function can:
/// - modify the address data, or
/// - delete the address.
///
/// # Security
///
/// Adjusts untrusted last seen times so they are not in the future. This stops
/// malicious peers keeping all their addresses at the front of the connection
/// queue. Honest peers with future clock skew also get adjusted.
///
/// Rejects all addresses if any calculated times overflow or underflow.
pub(crate) fn validate_addrs(
    addrs: impl IntoIterator<Item = MetaAddr>,
    last_seen_limit: DateTime32,
) -> impl Iterator<Item = MetaAddr> {
    // Note: The address book handles duplicate addresses internally,
    // so we don't need to de-duplicate addresses here.

    // TODO:
    // We should eventually implement these checks in this function:
    // - Zebra should ignore peers that are older than 3 weeks (part of #1865)
    // - Zebra should count back 3 weeks from the newest peer timestamp sent
    //   by the other peer, to compensate for clock skew

    let mut addrs: Vec<_> = addrs.into_iter().collect();

    limit_last_seen_times(&mut addrs, last_seen_limit);

    addrs.into_iter()
}

/// Ensure all reported `last_seen` times are less than or equal to `last_seen_limit`.
///
/// This will consider all addresses as invalid if trying to offset their
/// `last_seen` times to be before the limit causes an underflow.
fn limit_last_seen_times(addrs: &mut Vec<MetaAddr>, last_seen_limit: DateTime32) {
    let last_seen_times = addrs.iter().map(|meta_addr| {
        meta_addr
            .untrusted_last_seen()
            .expect("unexpected missing last seen: should be provided by deserialization")
    });
    let oldest_seen = last_seen_times.clone().min().unwrap_or(DateTime32::MIN);
    let newest_seen = last_seen_times.max().unwrap_or(DateTime32::MAX);

    // If any time is in the future, adjust all times, to compensate for clock skew on honest peers
    if newest_seen > last_seen_limit {
        let offset = newest_seen
            .checked_duration_since(last_seen_limit)
            .expect("unexpected underflow: just checked newest_seen is greater");

        // Check for underflow
        if oldest_seen.checked_sub(offset).is_some() {
            // No underflow is possible, so apply offset to all addresses
            for addr in addrs {
                let last_seen = addr
                    .untrusted_last_seen()
                    .expect("unexpected missing last seen: should be provided by deserialization");
                let last_seen = last_seen
                    .checked_sub(offset)
                    .expect("unexpected underflow: just checked oldest_seen");

                addr.set_untrusted_last_seen(last_seen);
            }
        } else {
            // An underflow will occur, so reject all gossiped peers
            addrs.clear();
        }
    }
}
