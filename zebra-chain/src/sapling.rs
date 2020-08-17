//! Sapling-related functionality.

mod spend;
mod output;

pub use spend::Spend;
pub use output::Output;

// XXX clean up these modules

pub mod address;
pub mod commitment;
pub mod keys;
pub mod note;
pub mod tree;

#[cfg(test)]
mod tests;