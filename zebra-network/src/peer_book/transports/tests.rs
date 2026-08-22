//! Tests for learned transport reachability.

use std::time::{Duration, Instant};

use crate::PeerSocketAddr;

use super::{AddrTransports, TransportTable, TRANSPORT_FAILURE_RETRY_INTERVAL};

fn addr(host: u8) -> PeerSocketAddr {
    format!("10.0.0.{host}:8233")
        .parse()
        .expect("valid address")
}

#[test]
fn handshakes_establish_reachability_per_transport() {
    let _init_guard = zebra_test::init();

    let mut table = TransportTable::default();
    let now = Instant::now();

    table.record_reachable(addr(1), AddrTransports::TCP);
    assert_eq!(table.dialable(&addr(1), now), AddrTransports::TCP);

    table.record_reachable(addr(1), AddrTransports::QUIC);
    assert_eq!(
        table.dialable(&addr(1), now),
        AddrTransports::TCP | AddrTransports::QUIC,
        "a peer can be reachable on both transports",
    );

    // Other addresses are unaffected.
    assert_eq!(table.dialable(&addr(2), now), AddrTransports::empty());
}

#[test]
fn refusals_are_remembered_then_retried() {
    let _init_guard = zebra_test::init();

    let mut table = TransportTable::default();
    let now = Instant::now();

    table.record_reachable(addr(1), AddrTransports::TCP | AddrTransports::QUIC);
    table.record_unreachable(addr(1), AddrTransports::QUIC, now);

    assert_eq!(
        table.dialable(&addr(1), now),
        AddrTransports::TCP,
        "a refused transport is not dialed again immediately",
    );

    let later = now + TRANSPORT_FAILURE_RETRY_INTERVAL + Duration::from_secs(1);
    assert_eq!(
        table.dialable(&addr(1), later),
        AddrTransports::TCP | AddrTransports::QUIC,
        "peers upgrade, so refusals expire",
    );

    // A later handshake clears the refusal immediately.
    table.record_unreachable(addr(1), AddrTransports::QUIC, later);
    table.record_reachable(addr(1), AddrTransports::QUIC);
    assert_eq!(
        table.dialable(&addr(1), later),
        AddrTransports::TCP | AddrTransports::QUIC,
    );
}

#[test]
fn bans_remove_learned_reachability() {
    let _init_guard = zebra_test::init();

    let mut table = TransportTable::default();
    let now = Instant::now();
    table.record_reachable(addr(1), AddrTransports::QUIC);
    table.record_reachable(addr(2), AddrTransports::QUIC);

    table.remove_if(|entry| entry == addr(1));

    assert_eq!(table.dialable(&addr(1), now), AddrTransports::empty());
    assert_eq!(table.dialable(&addr(2), now), AddrTransports::QUIC);
}
