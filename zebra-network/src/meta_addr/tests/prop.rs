use super::super::*;

// XXX remove this test and replace it with a proptest instance.
#[test]
fn sanitize_truncates_timestamps() {
    zebra_test::init();

    let services = PeerServices::default();
    let addr = "127.0.0.1:8233".parse().unwrap();

    let entry = MetaAddr {
        services,
        addr,
        last_seen: Utc.timestamp(1_573_680_222, 0),
        last_connection_state: Responded,
    }
    .sanitize();

    // We want the sanitized timestamp to be a multiple of the truncation interval.
    assert_eq!(
        entry.get_last_seen().timestamp() % crate::constants::TIMESTAMP_TRUNCATION_SECONDS,
        0
    );
    // We want the state to be the default
    assert_eq!(entry.last_connection_state, Default::default());
    // We want the other fields to be unmodified
    assert_eq!(entry.addr, addr);
    assert_eq!(entry.services, services);
}
