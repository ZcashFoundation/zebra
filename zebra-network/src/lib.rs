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

// XXX make this private once connect is removed
pub mod meta_addr;
// XXX make this private once connect is removed
pub mod constants;
