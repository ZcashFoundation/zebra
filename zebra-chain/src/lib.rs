//! Blockchain-related datastructures for Zebra. 🦓
#![deny(missing_docs)]

#[macro_use]
extern crate failure;

pub mod block;
pub mod serialization;
pub mod transaction;
pub mod types;
