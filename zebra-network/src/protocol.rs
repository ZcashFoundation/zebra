//! Zcash network protocol handling.

pub mod codec;
pub mod message;
pub mod types;

pub mod inv;

// XXX at some later point the above should move to an `external` submodule, so
// that we have
// - protocol::external::{all_bitcoin_zcash_types};
// - protocol::internal::{all_internal_req_rsp_types};

pub mod internal;
