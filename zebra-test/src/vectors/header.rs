//! Contiguous Mainnet header fixtures for headers-first workloads.

/// Two batches of 160 distinct, contiguous Mainnet headers.
///
/// Each entry is `(first_height, newline_delimited_header_hex)`. The ranges are
/// 1–160 (Sprout) and 1,687,106–1,687,265 (NU5). Both first headers also occur in
/// [`super::MAINNET_BLOCKS`], providing independent anchors for fixture consumers.
/// These are serialized headers, without the transaction-count byte used in a
/// P2P `headers` response. Decode them outside measured intervals.
///
/// Retrieved on 2026-11-12 using Mainnet P2P `getheaders` from 38.49.217.54:8233
/// (discovered through seeder.zec.rocks), at protocol version 170160. Locators
/// were the existing genesis hash and the previous-block hash of the existing
/// NU5 activation block. Request 160 headers, strip each trailing transaction
/// count, and hex-encode each header on its own line. No network is needed to
/// consume these fixtures.
///
/// Final hashes, in display order:
/// - Sprout: `00005047b18afe99f684e12ae316f707882a6cd91107679f5c87248b2e3ffcd1`.
/// - NU5: `0000000001b7d6fad4951aae2fa710631923d06febea5abe7acce113bedce2ca`.
pub const MAINNET_HEADER_BATCHES: &[(u32, &str)] = &[
    (1, include_str!("headers-main-0-000-001.txt")),
    (1_687_106, include_str!("headers-main-1-687-106.txt")),
];
