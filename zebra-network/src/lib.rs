//! Networking code for Zebra. 🦓

#![deny(missing_docs)]

#[macro_use]
extern crate failure;

pub mod message;
pub mod types;

// XXX make this private once connect is removed
pub mod meta_addr;
// XXX make this private once connect is removed
pub mod constants;
