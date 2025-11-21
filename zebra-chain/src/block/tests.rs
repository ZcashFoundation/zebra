//! Tests for Zebra blocks

#![allow(clippy::unwrap_in_result)]

// TODO: generate should be rewritten as strategies
#[cfg(any(test, feature = "bench", feature = "proptest-impl"))]
pub mod generate;
#[cfg(test)]
mod preallocate;
#[cfg(test)]
mod prop;
#[cfg(test)]
mod vectors;
