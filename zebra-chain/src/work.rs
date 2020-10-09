//! Proof-of-work implementation.

pub mod difficulty;
pub mod equihash;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(test)]
mod tests;
