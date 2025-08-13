//! Tower middleware for batch request processing
//!
//! This crate provides generic middleware for handling management of
//! latency/throughput tradeoffs for batch processing. It provides a
//! [`BatchControl<R>`](BatchControl) enum with [`Item(R)`](BatchControl::Item)
//! and [`Flush`](BatchControl::Flush) variants, and provides a
//! [`Batch<S>`](Batch) wrapper that wraps `S: Service<BatchControl<R>>` to
//! provide a `Service<R>`, managing maximum request latency and batch size.
//!
//! ## Example: batch verification
//!
//! In cryptography, batch verification asks whether *all* items in some set are
//! valid, rather than asking whether *each* of them is valid. This increases
//! throughput by allowing computation to be shared across each item. However, it
//! comes at the cost of higher latency (the entire batch must complete),
//! complexity of caller code (which must assemble a batch of items to verify),
//! and loss of the ability to easily pinpoint failing items (requiring either a
//! retry or more sophisticated techniques).
//!
//! The latency-throughput tradeoff is manageable, but the second aspect poses
//! serious practical difficulties. Conventional batch verification APIs require
//! choosing in advance how much data to batch, and then processing the entire
//! batch simultaneously. But for applications which require verification of
//! heterogeneous data, this is cumbersome and difficult.
//!
//! For example, Zcash uses four different kinds of signatures (ECDSA signatures
//! from Bitcoin, Ed25519 signatures for Sprout, and RedJubjub spendauth and
//! binding signatures for Sapling) as well as three different kinds of
//! zero-knowledge proofs (Sprout-on-BCTV14, Sprout-on-Groth16, and
//! Sapling-on-Groth16). A single transaction can have multiple proofs or
//! signatures of different kinds, depending on the transaction version and its
//! structure. Verification of a transaction conventionally proceeds
//! "depth-first", checking that the structure is appropriate and then that all
//! the component signatures and proofs are valid.
//!
//! Now consider the problem of implementing batch verification in this context,
//! using conventional batch verification APIs that require passing a list of
//! signatures or proofs. This is quite complicated, requiring implementing a
//! second transposed set of validation logic that proceeds "breadth-first",
//! checking that the structure of each transaction is appropriate while
//! assembling collections of signatures and proofs to verify. This transposed
//! validation logic must match the untransposed logic, but there is another
//! problem, which is that the set of transactions must be decided in advance.
//! This is difficult because different levels of batching are required in
//! different contexts. For instance, batching within a transaction is
//! appropriate on receipt of a gossiped transaction, batching within a block is
//! appropriate for block verification, and batching across blocks is appropriate
//! when syncing the chain.
//!
//! ## Asynchronous batch verification
//!
//! To address this problem, we move from a synchronous model for signature
//! verification to an asynchronous model. Rather than immediately returning a
//! verification result, verification returns a future which will eventually
//! resolve to a verification result. Verification futures can be combined with
//! various futures combinators, expressing the logical semantics of the combined
//! verification checks. This allows writing checks generic over the choice of
//! singleton or batched verification. And because the batch context is distinct
//! from the verification logic itself, the same verification logic can be reused
//! in different batching contexts - batching within a transaction, within a
//! block, within a chain, etc.
//!
//! ## Batch processing middleware
//!
//! Tower's [`Service`](tower::Service) interface is an attractive choice for
//! implementing this model for two reasons. First, it makes it easy to express
//! generic bounds on [`Service`](tower::Service)s, allowing higher-level
//! verification services to be written generically with respect to the
//! verification of each lower-level component.
//!
//! Second, Tower's design allows service combinators to easily compose
//! behaviors. For instance, the third drawback mentioned above (failure
//! pinpointing) can addressed fairly straightforwardly by composing a batch
//! verification [`Service`](tower::Service) with a retry
//! [`Layer`](tower::layer::Layer) that retries verification of that item without
//! batching.
//!
//! The remaining problem to address is the latency-throughput tradeoff. The
//! logic to manage this tradeoff is independent of the specific batching
//! procedure, and this crate provides a generic `Batch` wrapper that does so.
//! The wrapper makes use of a [`BatchControl<R>`](BatchControl) enum with
//! [`Item(R)`](BatchControl::Item) and [`Flush`](BatchControl::Flush) variants.
//! Given `S: Service<BatchControl<R>>`, the [`Batch<S>`](Batch) wrapper provides
//! a `Service<R>`. The wrapped service does not need to implement any batch
//! control logic, as it will receive explicit [`Flush`](BatchControl::Flush)
//! requests from the wrapper.
//!
//! ## Implementation History
//!
//! The `tower-batch-control` code was modified from a 2019 version of:
//! <https://github.com/tower-rs/tower/tree/master/tower/src/buffer>
//!
//! A modified fork of this crate is available on crates.io as `tower-batch`.
//! It is focused on batching disk writes.

pub mod error;
pub mod future;
mod layer;
mod message;
mod service;
mod worker;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Signaling mechanism for batchable services that allows explicit flushing.
///
/// This request type is a generic wrapper for the inner `Req` type.
pub enum BatchControl<Req: RequestWeight> {
    /// A new batch item.
    Item(Req),
    /// The current batch should be flushed.
    Flush,
}

impl<Req: RequestWeight> From<Req> for BatchControl<Req> {
    fn from(req: Req) -> BatchControl<Req> {
        BatchControl::Item(req)
    }
}

pub use self::layer::BatchLayer;
pub use self::service::Batch;

/// A trait for reading the weight of a request to [`BatchControl`] services.
pub trait RequestWeight {
    /// Returns the weight of a request relative to the maximum threshold for flushing
    /// requests to the underlying service.
    fn request_weight(&self) -> usize {
        1
    }
}

// [`RequestWeight`] impls for test `Item` types
impl RequestWeight for () {}
impl RequestWeight for &'static str {}
