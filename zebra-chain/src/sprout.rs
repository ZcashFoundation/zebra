//! Sprout-related functionality.

mod joinsplit;
pub use joinsplit::JoinSplit;

// XXX clean up these modules

pub mod address;
pub mod commitment;
pub mod keys;
pub mod note;
pub mod tree;

#[cfg(test)]
mod tests;
