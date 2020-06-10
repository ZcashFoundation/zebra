//! Block and transaction verification for Zebra.
//!
//! Verification is provided via `tower::Service`s, to support backpressure and batch
//! verification.

mod block;
mod redjubjub;
mod script;
mod transaction;

pub use block::init;
pub use redjubjub::{BatchVerifier, Request, SingletonVerifier};
