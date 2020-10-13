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

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_consensus")]
// Re-enable this after cleaning the API surface.
//#![deny(missing_docs)]
#![allow(clippy::try_err)]

pub mod block;
pub mod chain;
pub mod checkpoint;
pub mod config;
pub mod error;
pub mod mempool;
pub mod parameters;
pub mod script;

#[allow(dead_code)] // Remove this once transaction verification is implemented
mod primitives;
mod transaction;

pub use crate::config::Config;

/// A boxed [`std::error::Error`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
