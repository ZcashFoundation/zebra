//! The version 2 Zcash P2P network protocol.
//!
//! This module implements the wire formats of the draft ZIP
//! "Version 2 Zcash P2P Network Protocol": a successor to the legacy
//! Bitcoin-inherited message protocol, defined against a transport-neutral
//! stream layer realized by QUIC.
//!
//! Implemented against the draft revision pinned in `SPEC-CONFORMANCE.md`
//! (zcash/zips PR #1346 at `a3f4fa2a`, which includes the PR #1344 transport
//! draft). The drafts are still moving: that file maps each draft section to
//! its implementing module, tests, and conformance status, and must be
//! updated whenever the implemented revision changes.
//!
//! All application data is carried on streams typed by their first byte:
//! - a single bidirectional *handshake stream* (type `0x00`) carrying
//!   length-prefixed records, starting with the [`init`] record exchange,
//! - bidirectional *request streams* (types `0x01`–`0x09`), each carrying
//!   exactly one request and its response, and
//! - long-lived unidirectional *announcement streams* (types `0x10`–`0x12`)
//!   carrying length-prefixed announcement records.
//!
//! This module contains the wire encodings and their limits, and the QUIC
//! transport that carries them; the peer connection logic lives elsewhere.

pub mod compact_block;
pub mod constants;
pub mod init;
pub mod quic;
pub mod record;
pub mod request;
pub mod response;
pub mod txref;
pub mod types;

#[cfg(test)]
mod tests;
