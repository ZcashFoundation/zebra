//! Implementation of Zcash consensus checks.
//!
//! More specifically, this crate implements *semantic* validity checks,
//! as defined below.
//!
//! ## Verification levels.
//!
//! Zebra's implementation of the Zcash consensus rules is oriented
//! around three telescoping notions of validity:
//!
//! 1. *Structural Validity*, or whether the format and structure of the
//!    object are valid.  For instance, Sprout-on-BCTV14 proofs are not
//!    allowed in version 4 transactions, and a transaction with a spend
//!    or output description must include a binding signature.
//!
//! 2. *Semantic Validity*, or whether the object could potentially be
//!    valid, depending on the chain state.  For instance, a transaction
//!    that spends a UTXO must supply a valid unlock script; a shielded
//!    transaction must have valid proofs, etc.
//!
//! 3. *Contextual Validity*, or whether a semantically valid
//!    transaction is actually valid in the context of a particular
//!    chain state.  For instance, a transaction that spends a
//!    UTXO is only valid if the UTXO remains unspent; a
//!    shielded transaction spending some note must reveal a nullifier
//!    not already in the nullifier set, etc.
//!
//! *Structural validity* is enforced by the definitions of data
//! structures in `zebra-chain`.  *Semantic validity* is enforced by the
//! code in this crate.  *Contextual validity* is enforced in
//! `zebra-state` when objects are committed to the chain state.

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_consensus")]
// Re-enable this after cleaning the API surface.
//#![deny(missing_docs)]
#![allow(clippy::try_err)]
// Disable some broken or unwanted clippy nightly lints
// Build without warnings on nightly 2021-01-17 and later and stable 1.51 and later
#![allow(unknown_lints)]
// Disable old lint warnings on nightly until 1.51 is stable
#![allow(renamed_and_removed_lints)]
// Use the old lint name to build without warnings on stable until 1.51 is stable
#![allow(clippy::unknown_clippy_lints)]
// The actual lints we want to disable
#![allow(clippy::unnecessary_wraps)]

mod block;
mod checkpoint;
mod config;
#[allow(dead_code)]
mod parameters;
// #[allow(dead_code)] // Remove this once transaction verification is implemented
mod primitives;
mod script;
mod transaction;

pub mod chain;
pub mod error;

pub use checkpoint::MAX_CHECKPOINT_BYTE_COUNT;
pub use checkpoint::MAX_CHECKPOINT_HEIGHT_GAP;
pub use config::Config;

/// A boxed [`std::error::Error`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
