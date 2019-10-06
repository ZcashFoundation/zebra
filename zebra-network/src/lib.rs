//! Networking code for Zebra. 🦓

#![deny(missing_docs)]

#[macro_use]
extern crate failure;
#[macro_use]
extern crate tracing;
#[macro_use]
extern crate bitflags;

mod network;
pub use network::Network;

pub mod protocol;

// XXX revisit privacy once we finish encapsulation.
pub mod address_book;
pub mod constants;
pub mod meta_addr;
pub mod peer;
