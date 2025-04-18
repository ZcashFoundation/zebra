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

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_consensus")]

mod block;
mod checkpoint;
mod primitives;
mod script;

pub mod config;
pub mod error;
pub mod router;
pub mod transaction;

#[cfg(any(test, feature = "proptest-impl"))]
pub use block::check::difficulty_is_valid;

pub use block::{
    subsidy::{
        funding_streams::{funding_stream_address, funding_stream_values, new_coinbase_script},
        general::{block_subsidy, miner_subsidy},
    },
    Request, VerifyBlockError, MAX_BLOCK_SIGOPS,
};
pub use checkpoint::{
    list::ParameterCheckpoint, CheckpointList, VerifyCheckpointError, MAX_CHECKPOINT_BYTE_COUNT,
    MAX_CHECKPOINT_HEIGHT_GAP,
};
pub use config::Config;
pub use error::BlockError;
pub use primitives::{ed25519, groth16, halo2, redjubjub, redpallas};
pub use router::RouterError;

/// A boxed [`std::error::Error`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

mod orchard_zsa;
