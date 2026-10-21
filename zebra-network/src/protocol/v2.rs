//! The version 2 Zcash peer-to-peer protocol.
//!
//! Implements the draft ZIP "Version 2 Zcash P2P Network Protocol"
//! ([`zips/draft-arya-jvff-p2p-quic-transport.md`][draft]), pinned at
//! zcash/zips commit `65ec4f743` (2026-08-20). Where this module cites a
//! section, it is a section of that revision.
//!
//! Unlike the legacy protocol in [`crate::protocol::external`], version 2 is
//! not a message set on a TCP byte stream: it is a stream layer carried over
//! a secure transport (QUIC, and eventually Tor), where the first byte of
//! every stream selects its type and the connection handshake is a single
//! `init` record exchange. There is no compatibility bridge between the two
//! protocols; a node runs both during the transition (draft, "Deployment").
//!
//! [draft]: https://github.com/zcash/zips/blob/65ec4f743220185df56088d900f1e20134a00c15/zips/draft-arya-jvff-p2p-quic-transport.md

pub mod announcement;
pub mod constants;
pub mod handshake;
pub mod record;
pub mod request;
pub mod response;
pub mod txref;
pub mod types;
pub mod wire;

#[cfg(test)]
mod tests;
