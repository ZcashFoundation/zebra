//! Shared helpers for zebra-consensus benchmarks.
//!
//! Each bench file under `benches/` is its own crate, so helpers shared across
//! them live here and are wired in via `mod common;` at the top of each bench.

#![allow(dead_code)]

/// Creates a batch of `n` items by cycling through `source`.
///
/// The source test vectors in `zebra-test` contain only a handful of real
/// proofs/bundles. To benchmark realistic batch sizes, items are repeated.
/// This is valid for cost-per-proof measurements because proof verification
/// runs the same curve operations regardless of proof content, but it does
/// not capture the memory/cache patterns of fully unique inputs.
pub fn cycled<T: Clone>(source: &[T], n: usize) -> Vec<T> {
    source.iter().cycle().take(n).cloned().collect()
}
