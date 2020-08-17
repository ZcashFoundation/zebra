//! Sapling-related functionality.

mod output;
mod spend;

pub use output::Output;
pub use spend::Spend;

// XXX clean up these modules

pub mod address;
pub mod commitment;
pub mod keys;
pub mod note;
pub mod tree;

#[cfg(test)]
mod tests;
