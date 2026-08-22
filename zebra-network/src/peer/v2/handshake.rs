//! The application handshake of the version 2 Zcash P2P network protocol.
//!
//! Immediately after the transport connection is established, the initiator
//! opens the dedicated handshake stream (stream type `0x00`) and the peers
//! exchange `init` records. The handshake is complete for a peer once it has
//! both sent and received an `init` record; the negotiated protocol version
//! is the minimum of the two advertised versions.
//!
//! The handshake stream remains open for the life of the connection.

use thiserror::Error;

use crate::{
    peer::handshake::HandshakeNonces,
    protocol::{
        external::types::Version,
        v2::{
            init::InitRecord,
            record,
            types::{ErrorCode, StreamType, WireError},
        },
    },
};

/// An error during the version 2 application handshake.
#[derive(Error, Debug)]
pub enum V2HandshakeError {
    /// The remote peer's protocol version is below the minimum for this
    /// protocol or the current network epoch.
    #[error("remote peer protocol version {0:?} is obsolete")]
    ObsoleteVersion(Version),

    /// The remote peer sent a nonce that this node recently sent: the
    /// connection is to the node itself.
    #[error("connection to self detected")]
    SelfConnection,

    /// This node generated a nonce that collided with one already in use.
    #[error("local nonce already used by another connection")]
    LocalDuplicateNonce,

    /// The remote peer finished the handshake stream before sending an
    /// `init` record, signalling intent to disconnect.
    #[error("remote peer closed the handshake stream before sending an init record")]
    ConnectionClosed,

    /// A wire-format error reading the remote peer's handshake records.
    #[error("handshake wire error: {0}")]
    Wire(#[from] WireError),

    /// The transport connection failed during the handshake.
    #[error("connection error during handshake: {0}")]
    Connection(#[from] quinn::ConnectionError),

    /// A stream write failed during the handshake.
    #[error("stream write error during handshake: {0}")]
    Write(#[from] quinn::WriteError),
}

impl V2HandshakeError {
    /// Returns the application error code to close the connection with, if
    /// this error requires closing the connection.
    pub fn connection_error_code(&self) -> Option<ErrorCode> {
        match self {
            V2HandshakeError::ObsoleteVersion(_) => Some(ErrorCode::Obsolete),
            V2HandshakeError::SelfConnection => Some(ErrorCode::SelfConnection),
            V2HandshakeError::LocalDuplicateNonce => Some(ErrorCode::InternalError),
            V2HandshakeError::ConnectionClosed => Some(ErrorCode::NoError),
            V2HandshakeError::Wire(wire) => wire.connection_error_code(),
            V2HandshakeError::Connection(_) | V2HandshakeError::Write(_) => None,
        }
    }
}

/// The shared parameters of a version 2 handshake.
#[derive(Clone, Debug)]
pub struct HandshakeParams {
    /// The `init` record to send, containing a freshly generated nonce.
    pub local_init: InitRecord,

    /// The minimum acceptable remote protocol version: the maximum of the
    /// v2 protocol's minimum version and the version required by the current
    /// network epoch.
    pub min_remote_version: Version,

    /// The set of nonces recently sent in `init` records by this node, for
    /// self-connection detection. Shared between all connections.
    pub nonces: HandshakeNonces,

    /// The maximum number of nonces to keep in the shared set, bounding its
    /// memory use. Usually the configured total connection limit.
    pub nonce_limit: usize,
}

/// A completed version 2 handshake.
///
/// The handshake stream halves are kept open for the life of the connection:
/// finishing or resetting the handshake stream signals intent to disconnect.
pub struct V2Handshake {
    /// The remote peer's `init` record.
    pub remote_init: InitRecord,

    /// The negotiated protocol version:
    /// `min(local_version, remote_version)`.
    pub negotiated_version: Version,

    /// The sending half of the handshake stream.
    pub handshake_send: quinn::SendStream,

    /// The receiving half of the handshake stream.
    pub handshake_recv: quinn::RecvStream,
}

impl std::fmt::Debug for V2Handshake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("V2Handshake")
            .field("remote_init", &self.remote_init)
            .field("negotiated_version", &self.negotiated_version)
            .finish()
    }
}

/// Performs the version 2 handshake as the connection initiator:
/// opens the handshake stream, sends the local `init` record, and reads and
/// validates the remote peer's `init` record.
///
/// On validation failure, closes the connection with the applicable error
/// code. The caller should apply an overall handshake timeout.
pub async fn initiate(
    connection: &quinn::Connection,
    params: &HandshakeParams,
) -> Result<V2Handshake, V2HandshakeError> {
    register_local_nonce(params).await?;

    let handshake = async {
        let (mut handshake_send, handshake_recv) = connection.open_bi().await?;

        handshake_send
            .write_all(&[StreamType::Handshake.byte()])
            .await?;
        params.local_init.write(&mut handshake_send).await?;

        read_and_validate_remote_init(handshake_send, handshake_recv, params).await
    }
    .await;

    finish_handshake(connection, handshake)
}

/// Performs the version 2 handshake as the connection responder:
/// accepts streams until the handshake stream arrives (refusing any other
/// stream opened before the handshake), sends the local `init` record, and
/// reads and validates the remote peer's `init` record.
///
/// On validation failure, closes the connection with the applicable error
/// code. The caller should apply an overall handshake timeout.
pub async fn respond(
    connection: &quinn::Connection,
    params: &HandshakeParams,
) -> Result<V2Handshake, V2HandshakeError> {
    register_local_nonce(params).await?;

    let handshake = async {
        let (mut handshake_send, handshake_recv) = loop {
            let (send, mut recv) = connection.accept_bi().await?;

            match record::read_u8(&mut recv).await {
                Ok(type_byte) if type_byte == StreamType::Handshake.byte() => {
                    break (send, recv);
                }
                Ok(_other_stream_type) => {
                    // A stream received before the handshake completes may
                    // be refused; the peer can retry it afterwards.
                    refuse_stream(send, recv);
                }
                // The stream was reset before its type byte arrived.
                Err(_) => drop((send, recv)),
            }
        };

        // Send the local init record as soon as the handshake stream's type
        // byte has been observed, without waiting for the remote init record.
        params.local_init.write(&mut handshake_send).await?;

        read_and_validate_remote_init(handshake_send, handshake_recv, params).await
    }
    .await;

    finish_handshake(connection, handshake)
}

/// Reads the remote `init` record and validates it against `params`.
async fn read_and_validate_remote_init(
    handshake_send: quinn::SendStream,
    mut handshake_recv: quinn::RecvStream,
    params: &HandshakeParams,
) -> Result<V2Handshake, V2HandshakeError> {
    let remote_init = InitRecord::read(&mut handshake_recv)
        .await?
        .ok_or(V2HandshakeError::ConnectionClosed)?;

    if remote_init.version < params.min_remote_version {
        return Err(V2HandshakeError::ObsoleteVersion(remote_init.version));
    }

    // Check the remote nonce against every nonce this node recently sent:
    // any match means the connection is to the node itself, possibly via a
    // simultaneous connection in each direction.
    if params.nonces.contains(&remote_init.nonce) {
        return Err(V2HandshakeError::SelfConnection);
    }

    let negotiated_version = std::cmp::min(params.local_init.version, remote_init.version);

    Ok(V2Handshake {
        remote_init,
        negotiated_version,
        handshake_send,
        handshake_recv,
    })
}

/// Inserts the local nonce into the shared nonce set, bounding the set's
/// size.
async fn register_local_nonce(params: &HandshakeParams) -> Result<(), V2HandshakeError> {
    if !params
        .nonces
        .register(params.local_init.nonce, params.nonce_limit)
    {
        return Err(V2HandshakeError::LocalDuplicateNonce);
    }

    Ok(())
}

/// Closes the connection with the applicable error code if the handshake
/// failed.
fn finish_handshake(
    connection: &quinn::Connection,
    handshake: Result<V2Handshake, V2HandshakeError>,
) -> Result<V2Handshake, V2HandshakeError> {
    if let Err(error) = &handshake {
        if let Some(code) = error.connection_error_code() {
            connection.close(code.into(), error.to_string().as_bytes());
        }

        // The local nonce is deliberately not evicted here, even though this
        // handshake will not use it again: see the security note on
        // [`HandshakeNonces::contains`].
    }

    handshake
}

/// Refuses a bidirectional stream: cancels the peer's sending direction and
/// resets the local sending direction, both with the `REFUSED` error code.
fn refuse_stream(mut send: quinn::SendStream, mut recv: quinn::RecvStream) {
    let _ = recv.stop(ErrorCode::Refused.into());
    let _ = send.reset(ErrorCode::Refused.into());
}

#[cfg(test)]
mod tests;
