//! The version 2 connection handshake (draft, "Connection Handshake").
//!
//! On success the handshake stream is handed to the connection, which keeps
//! it open for the connection's life. On failure the connection is closed
//! with the draft's error code before the error is returned.

use std::{cmp::min, sync::Arc};

use chrono::Utc;
use futures::StreamExt;
use quinn::{Connection, RecvStream, SendStream};
use thiserror::Error;
use tokio::time::Instant;
use tokio_util::codec::FramedRead;

use zebra_chain::{block, chain_tip::ChainTip, serialization::SerializationError};

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ConnectedAddr, ConnectionInfo, MinimumPeerVersion, RemoteHandshake},
    protocol::{
        external::types::{Nonce, PeerServices, Version},
        v2::{
            handshake::{decode_record, InitRecord},
            record::RecordCodec,
            types::{ErrorCode, StreamType},
            wire::WireError,
        },
    },
    quic::app_error_code,
};

use super::{
    connection::refuse_stream,
    nonces::{NonceRegistry, NonceRegistryError},
};

#[cfg(test)]
mod tests;

/// Which side of the connection this node is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Role {
    /// This node opened the connection, and opens the handshake stream.
    Initiator,

    /// This node accepted the connection, and waits for the handshake
    /// stream.
    Responder,
}

/// What this node says about itself in its `init` record.
///
/// The user agent is at most
/// [`MAX_USER_AGENT_LENGTH`](crate::protocol::external::MAX_USER_AGENT_LENGTH)
/// bytes, checked once at startup by `peer_set::initialize::v2::start`.
#[derive(Clone, Debug)]
pub struct LocalIdentity {
    /// The user agent.
    pub(crate) user_agent: String,

    /// The services this node advertises.
    pub(crate) services: PeerServices,

    /// Whether this node wants transaction relay from the peer.
    pub(crate) relay: bool,
}

impl LocalIdentity {
    /// The `init` record this node sends, with a fresh nonce and the current
    /// chain tip height.
    ///
    /// Compact block announcements are not implemented, so `announce` and
    /// `full_ids` are 0.
    fn init_record(&self, nonce: Nonce, start_height: block::Height) -> InitRecord {
        InitRecord {
            version: CURRENT_NETWORK_PROTOCOL_VERSION,
            services: self.services,
            nonce,
            user_agent: self.user_agent.clone(),
            start_height,
            relay: self.relay,
            announce: false,
            full_ids: false,
        }
    }
}

/// A completed handshake.
pub struct CompletedHandshake {
    /// The peer's `init` record and the negotiated version.
    pub connection_info: Arc<ConnectionInfo>,

    /// This node's side of the handshake stream, kept open by the
    /// connection: finishing it would signal intent to disconnect.
    pub send: SendStream,

    /// The peer's side, framed into records, which the connection reads for
    /// its whole life: a second `init` is a `PROTOCOL_ERROR`, records of other
    /// kinds are ignored, and the peer's FIN or reset means it intends to
    /// disconnect (draft, "Handshake Sequence", "Handshake Validation").
    pub recv: FramedRead<RecvStream, RecordCodec>,
}

/// A failed handshake.
///
/// [`HandshakeError::error_code`] is the code the connection was closed
/// with.
#[derive(Error, Debug)]
pub enum HandshakeError {
    /// The handshake did not complete before the deadline.
    #[error("the handshake did not complete in time")]
    Timeout,

    /// The peer's `init` record could not be decoded (draft, "Handshake
    /// Validation": `PROTOCOL_ERROR`).
    #[error("invalid init record from peer: {0}")]
    Init(#[from] SerializationError),

    /// The peer's version is below the minimum for the protocol and the
    /// current network epoch (draft, "Handshake Validation": `OBSOLETE`).
    #[error("peer advertised obsolete version {version}, minimum is {min_version}")]
    Obsolete {
        /// The version the peer advertised.
        version: Version,
        /// The minimum version this node accepts.
        min_version: Version,
    },

    /// A record on the handshake stream could not be framed: an oversized
    /// length prefix, a stream finished mid-record, this node's own record
    /// over the limit, which is a local bug, or the stream failed or was
    /// reset while reading, a transport failure closed with `NO_ERROR`.
    #[error("handshake stream framing error: {0}")]
    Record(#[from] WireError),

    /// The peer's nonce is one this node sent: the connection is to itself.
    #[error("the connection is to this node itself")]
    SelfConnection,

    /// An outbound peer does not serve blocks while this node is syncing
    /// (the legacy handshake's rule, #11061).
    #[error("outbound peer is missing required services, advertised: {services:?}")]
    MissingRequiredServices {
        /// The services the peer advertised.
        services: PeerServices,
    },

    /// The nonce registry was not reachable.
    #[error("nonce registry error: {0}")]
    Nonces(#[from] NonceRegistryError),

    /// The peer finished its side of the handshake stream before sending an
    /// `init` record: it does not want to continue.
    #[error("the peer finished the handshake stream before sending init")]
    StreamFinished,

    /// The peer finished a stream before sending a complete type byte
    /// (draft, "Stream Types": a connection error of type `PROTOCOL_ERROR`).
    #[error("the peer finished a stream before its type byte")]
    FinishedBeforeStreamType,

    /// The QUIC connection failed or was closed by the peer.
    #[error("connection error: {0}")]
    Connection(#[from] quinn::ConnectionError),

    /// Writing to the handshake stream failed.
    #[error("could not write to the handshake stream: {0}")]
    Write(#[from] quinn::WriteError),

    /// Reading a stream's type byte failed.
    #[error("could not read a stream type: {0}")]
    Read(#[from] quinn::ReadError),
}

impl HandshakeError {
    /// The application error code the connection is closed with: `NO_ERROR`
    /// for anything not attributable to the peer (transport failures, a
    /// finished stream, a declined peer, a timeout).
    pub fn error_code(&self) -> ErrorCode {
        match self {
            HandshakeError::Init(_) => ErrorCode::ProtocolError,
            HandshakeError::Obsolete { .. } => ErrorCode::Obsolete,
            HandshakeError::Record(error) => error.close_code(),
            HandshakeError::SelfConnection => ErrorCode::SelfConnection,
            HandshakeError::FinishedBeforeStreamType => ErrorCode::ProtocolError,
            HandshakeError::Nonces(_) => ErrorCode::InternalError,
            HandshakeError::Timeout
            | HandshakeError::StreamFinished
            | HandshakeError::MissingRequiredServices { .. }
            | HandshakeError::Connection(_)
            | HandshakeError::Write(_)
            | HandshakeError::Read(_) => ErrorCode::NoError,
        }
    }
}

/// Runs the handshake on `connection` as `role` before `deadline`, measured
/// from the transport connection's establishment, as the draft asks. On
/// failure the connection is closed with the error's code.
pub async fn handshake<C: ChainTip>(
    connection: &Connection,
    connected_addr: ConnectedAddr,
    role: Role,
    identity: &LocalIdentity,
    minimum_peer_version: &mut MinimumPeerVersion<C>,
    nonces: &NonceRegistry,
    deadline: Instant,
) -> Result<CompletedHandshake, HandshakeError> {
    let result = tokio::time::timeout_at(
        deadline,
        run(
            connection,
            connected_addr,
            role,
            identity,
            minimum_peer_version,
            nonces,
        ),
    )
    .await
    .unwrap_or(Err(HandshakeError::Timeout));

    if let Err(error) = &result {
        connection.close(app_error_code(error.error_code()), b"handshake failed");
    }

    result
}

/// The handshake proper, without the deadline.
async fn run<C: ChainTip>(
    connection: &Connection,
    connected_addr: ConnectedAddr,
    role: Role,
    identity: &LocalIdentity,
    minimum_peer_version: &mut MinimumPeerVersion<C>,
    nonces: &NonceRegistry,
) -> Result<CompletedHandshake, HandshakeError> {
    let local_nonce = Nonce::default();

    // Whether this node is still syncing, which decides whether outbound peers must serve blocks.
    let is_syncing = !minimum_peer_version
        .chain_tip()
        .is_at_or_near_network_tip(Utc::now());

    let start_height = minimum_peer_version
        .chain_tip()
        .best_tip_height()
        .unwrap_or(block::Height(0));
    let our_init = identity.init_record(local_nonce, start_height);
    let our_record = RecordCodec::encode_record(our_init.encode())?;

    let (mut send, recv) = match role {
        Role::Initiator => {
            let (mut send, recv) = connection.open_bi().await?;
            send.write_all(&[StreamType::Handshake.code()]).await?;
            (send, recv)
        }
        Role::Responder => accept_handshake_stream(connection).await?,
    };

    // Registered just before the record is written, so a peer that is this node sees the nonce
    // when it asks (commands are handled in order), and a peer that never opens the handshake
    // stream costs no registry slot.
    nonces.register(local_nonce).await?;

    // The responder may send its init as soon as it has seen the type byte, without waiting
    // for the initiator's record (draft, "Handshake Sequence").
    send.write_all(&our_record).await?;

    let mut recv = FramedRead::new(recv, RecordCodec);
    let remote = loop {
        let payload = recv.next().await.ok_or(HandshakeError::StreamFinished)??;
        match decode_record(&payload)? {
            Some(init) => break init,
            None => {
                // `decode_record` reads the kind byte, so the payload is not empty.
                tracing::debug!(
                    kind = payload[0],
                    "ignoring handshake record of unknown kind"
                );
            }
        }
    };

    // The draft leaves the protocol's minimum version unassigned, so the minimum is the current
    // network epoch's, the same comparison as the legacy handshake: a peer the legacy handshake
    // accepts is accepted here too.
    let min_version = minimum_peer_version.current();
    if remote.version < min_version {
        return Err(HandshakeError::Obsolete {
            version: remote.version,
            min_version,
        });
    }

    if nonces.contains(remote.nonce).await? {
        return Err(HandshakeError::SelfConnection);
    }

    // # Security
    // While syncing, require `NODE_NETWORK` from outbound peers, as the legacy handshake does
    // (#11061): peers without it would hold outbound slots and receive block requests they
    // cannot answer.
    if is_syncing
        && role == Role::Initiator
        && !remote.services.contains(PeerServices::NODE_NETWORK)
    {
        metrics::counter!(
            "zcash.net.peers.missing_services",
            "remote_ip" => connected_addr.get_transient_addr_label(),
            "remote_services" => format!("{:?}", remote.services),
            "user_agent" => remote.user_agent.clone(),
        )
        .increment(1);
        return Err(HandshakeError::MissingRequiredServices {
            services: remote.services,
        });
    }

    let negotiated_version = min(CURRENT_NETWORK_PROTOCOL_VERSION, remote.version);

    tracing::debug!(
        ?connected_addr,
        remote_version = ?remote.version,
        ?negotiated_version,
        ?min_version,
        user_agent = ?remote.user_agent,
        "completed version 2 handshake",
    );

    // The same metrics as the legacy handshake, so dashboards see both protocols.
    let remote_ip = connected_addr.get_transient_addr_label();
    metrics::counter!(
        "zcash.net.peers.connected",
        "remote_ip" => remote_ip.clone(),
        "remote_version" => remote.version.to_string(),
        "negotiated_version" => negotiated_version.to_string(),
        "min_version" => min_version.to_string(),
        "user_agent" => remote.user_agent.clone(),
    )
    .increment(1);
    metrics::gauge!("zcash.net.peers.version.connected", "remote_ip" => remote_ip)
        // `u32` to `f64` is lossless.
        .set(remote.version.0 as f64);

    Ok(CompletedHandshake {
        connection_info: Arc::new(ConnectionInfo {
            connected_addr,
            remote: RemoteHandshake::Init(remote),
            negotiated_version,
        }),
        send,
        recv,
    })
}

/// Accepts bidirectional streams until the handshake stream arrives,
/// refusing any other stream opened first: the draft allows buffering or
/// refusing, and a conforming initiator opens none before its `init`. A
/// known type is refused with `REFUSED`, an unknown one with
/// `UNSUPPORTED_STREAM_TYPE` (draft, "Stream Types").
async fn accept_handshake_stream(
    connection: &Connection,
) -> Result<(SendStream, RecvStream), HandshakeError> {
    loop {
        let (mut send, mut recv) = connection.accept_bi().await?;

        let mut stream_type = [0u8; 1];
        match recv.read_exact(&mut stream_type).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(_)) => {
                return Err(HandshakeError::FinishedBeforeStreamType)
            }
            Err(quinn::ReadExactError::ReadError(error)) => return Err(error.into()),
        }

        if stream_type[0] == StreamType::Handshake.code() {
            return Ok((send, recv));
        }

        let code = match StreamType::from_code(stream_type[0]) {
            Some(_) => ErrorCode::Refused,
            None => ErrorCode::UnsupportedStreamType,
        };
        tracing::debug!(
            stream_type = stream_type[0],
            ?code,
            "refusing stream opened before the handshake completed"
        );
        refuse_stream(Some(&mut send), &mut recv, code);
    }
}
