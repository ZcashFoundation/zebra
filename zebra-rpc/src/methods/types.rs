//! Types used in RPC methods.

mod get_blockchain_info;
pub mod grpc;
mod zec;

pub use get_blockchain_info::ValuePoolBalance;
pub use zec::Zec;
