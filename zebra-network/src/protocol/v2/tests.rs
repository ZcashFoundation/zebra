//! Tests for the version 2 Zcash P2P network protocol wire formats.

mod vectors;

/// Asserts that reading rejected the data as a `PROTOCOL_ERROR`.
#[track_caller]
fn assert_protocol_error<T: std::fmt::Debug>(
    result: &Result<T, super::types::WireError>,
    what: &str,
) {
    assert!(
        matches!(result, Err(super::types::WireError::Protocol(_))),
        "{what} must be a protocol error, got: {result:?}",
    );
}

/// Asserts that reading rejected the data as a `FLOOD` error.
#[track_caller]
fn assert_flood_error<T: std::fmt::Debug>(result: &Result<T, super::types::WireError>, what: &str) {
    assert!(
        matches!(result, Err(super::types::WireError::Flood(_))),
        "{what} must be a flood error, got: {result:?}",
    );
}
