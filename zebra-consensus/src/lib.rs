//! Consensus handling for Zebra.
//!
//! `verify::BlockVerifier` verifies blocks and their transactions, then adds them to
//! `zebra_state::ZebraState`.
//!
//! `mempool::MempoolTransactionVerifier` verifies transactions, and adds them to
//! `mempool::ZebraMempoolState`.
//!
//! Consensus handling is provided using `tower::Service`s, to support backpressure
//! and batch verification.

#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_consensus")]
#![deny(missing_docs)]

pub mod checkpoint;
pub mod mempool;
pub mod verify;
