//! Tests for version 2 misbehavior scores.

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use tokio::{sync::mpsc, time::Instant};

use super::{add_points, MisbehaviorScores};
use crate::{
    constants::{BAN_DURATION, MAX_PEER_MISBEHAVIOR_SCORE},
    PeerSocketAddr,
};

/// The draft's penalty for a malformed `get-headers` or `get-addr` response.
const POINTS: u32 = crate::protocol::v2::constants::RESPONSE_MISBEHAVIOR_PENALTY;

fn addr(addr: &str) -> PeerSocketAddr {
    addr.parse::<SocketAddr>().expect("valid address").into()
}

/// Scores add up across penalties from different connections of one peer
/// group, here two source ports of one IP: the group is reported with a full
/// score once it reaches the threshold, and only then. A penalty after the
/// report starts a new score.
#[tokio::test]
async fn scores_persist_across_connections_until_the_threshold() {
    let _init_guard = zebra_test::init();
    let (reports_tx, mut reports_rx) = mpsc::channel(8);
    let scores = MisbehaviorScores::spawn(reports_tx, 8);
    let first_connection = addr("203.0.113.7:8233");
    let second_connection = addr("203.0.113.7:40000");
    let other_peer = addr("203.0.113.8:8233");

    let penalties_to_ban = MAX_PEER_MISBEHAVIOR_SCORE.div_ceil(POINTS);
    assert!(penalties_to_ban > 1, "the test needs more than one penalty");

    // The other peer's penalty does not count towards the first peer's score.
    assert!(!scores.penalize(other_peer, POINTS, "other").await);

    // Most of the penalties on the first connection, the last on the second.
    for _ in 1..penalties_to_ban - 1 {
        assert!(!scores.penalize(first_connection, POINTS, "first").await);
    }
    assert!(!scores.penalize(second_connection, POINTS, "first").await);
    assert!(
        reports_rx.try_recv().is_err(),
        "nothing is reported below the threshold"
    );

    assert!(scores.penalize(second_connection, POINTS, "last").await);
    let report = reports_rx.try_recv().expect("the ban is reported");
    assert_eq!(report.addr, second_connection);
    assert_eq!(report.score, MAX_PEER_MISBEHAVIOR_SCORE);
    assert_eq!(report.reason, "last");

    // The ban replaces the score.
    assert!(!scores.penalize(first_connection, POINTS, "again").await);
    assert!(reports_rx.try_recv().is_err());
}

/// A score lapses a ban duration after its first penalty, and the map of
/// scores stays within its limit, dropping lapsed scores first, then the
/// oldest.
#[test]
fn scores_lapse_and_stay_bounded() {
    let _init_guard = zebra_test::init();
    let mut scores = HashMap::new();
    let [a, b, c] =
        ["203.0.113.1", "203.0.113.2", "203.0.113.3"].map(|ip| ip.parse().expect("valid IP"));
    let start = Instant::now();

    assert_eq!(add_points(&mut scores, a, POINTS, start, 2), POINTS);
    assert_eq!(add_points(&mut scores, a, POINTS, start, 2), 2 * POINTS);
    assert_eq!(
        add_points(&mut scores, a, POINTS, start + BAN_DURATION, 2),
        POINTS,
        "a lapsed score starts again"
    );

    let later = start + BAN_DURATION + Duration::from_secs(1);
    assert_eq!(add_points(&mut scores, b, POINTS, later, 2), POINTS);
    assert_eq!(
        add_points(&mut scores, c, POINTS, later + BAN_DURATION / 2, 2),
        POINTS
    );
    assert_eq!(scores.len(), 2, "the map stays within its limit");
    assert!(
        !scores.contains_key(&a),
        "the score whose window started first is dropped"
    );
}
