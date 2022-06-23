//! State contextual verification and storage code for Zebra. 🦓
//!
//! # Correctness
//!
//! Await UTXO and block commit requests should be wrapped in a timeout, because:
//! - await UTXO requests wait for a block containing that UTXO, and
//! - contextual verification and state updates wait for all previous blocks.
//!
//! Otherwise, verification of out-of-order and invalid blocks can hang indefinitely.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]

#[macro_use]
extern crate tracing;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
mod config;
pub mod constants;
mod error;
mod request;
mod response;
mod service;
mod util;

#[cfg(test)]
mod tests;

pub use config::{check_and_delete_old_databases, Config};
pub use constants::MAX_BLOCK_REORG_HEIGHT;
pub use error::{BoxError, CloneError, CommitBlockError, ValidateContextError};
pub use request::{FinalizedBlock, HashOrHeight, PreparedBlock, ReadRequest, Request};
pub use response::{ReadResponse, Response};
pub use service::{
    chain_tip::{ChainTipChange, LatestChainTip, TipAction},
    init, OutputIndex, OutputLocation, TransactionLocation,
};

#[cfg(any(test, feature = "proptest-impl"))]
pub use service::{
    arbitrary::populated_state,
    chain_tip::{ChainTipBlock, ChainTipSender},
    init_test, init_test_services,
};

pub(crate) use request::ContextuallyValidBlock;
