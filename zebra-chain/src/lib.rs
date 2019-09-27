//! Blockchain-related datastructures for Zebra. 🦓
#![deny(missing_docs)]

#[macro_use]
extern crate failure;

mod merkle_tree;
mod sha256d_writer;

pub mod block;
pub mod equihash_solution;
pub mod note_commitment_tree;
pub mod serialization;
pub mod transaction;
pub mod types;
