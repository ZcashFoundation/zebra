//! Unit tests for how the `get-object` requester classifies an answer that
//! ended before its `result` and `size`.

use std::io;

use bytes::Bytes;
use quinn::{ApplicationClose, ConnectionError, ReadError};

use crate::{
    protocol::v2::{types::ErrorCode, wire::WireError},
    quic::app_error_code,
};

use super::{refused_or_failure, Failure};

/// The error a reader returns for `error` on the stream.
fn read_error(error: ReadError) -> WireError {
    WireError::Io(io::Error::from(error))
}

/// Only the responder's reset of the request stream with `REFUSED` is a
/// refusal. Another reset code is not, and nor is a connection
/// the peer closes with the `REFUSED` code.
#[test]
fn only_a_refused_stream_reset_is_a_refusal() {
    let _init_guard = zebra_test::init();

    assert!(
        matches!(
            refused_or_failure(read_error(ReadError::Reset(app_error_code(
                ErrorCode::Refused
            )))),
            Failure::PeerRefused
        ),
        "a REFUSED reset is a refusal"
    );

    for (case, error) in [
        (
            "a CANCELLED reset",
            ReadError::Reset(app_error_code(ErrorCode::Cancelled)),
        ),
        (
            "a connection closed with REFUSED",
            ReadError::ConnectionLost(ConnectionError::ApplicationClosed(ApplicationClose {
                error_code: app_error_code(ErrorCode::Refused),
                reason: Bytes::from_static(b"REFUSED"),
            })),
        ),
    ] {
        let failure = refused_or_failure(read_error(error));
        assert!(
            matches!(failure, Failure::Transport(_)),
            "{case} is a transport failure, got {failure:?}"
        );
    }
}
