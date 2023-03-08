//! rkyv RPC method request structs

use bytecheck::CheckBytes;
use rkyv::{Archive, Deserialize, Serialize};

pub use crate::methods::{AddressStrings, Rpc};

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns software information from the RPC server, as a [`GetInfo`](super::response::GetInfo).
///
/// See [`Rpc::get_info`] for more information.
pub struct Info;

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns blockchain state information, as a [`GetBlockChainInfo`].
///
/// See [`Rpc::get_blockchain_info`] for more information.
pub struct BlockChainInfo;

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Total balance of a provided `addresses` in an [`AddressBalance`] instance.
///
/// See [`Rpc::get_address_balance`] for more information.
pub struct AddressBalance(pub AddressStrings);

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Sends the raw bytes of a signed transaction to the local node's mempool, if the transaction is valid.
///
/// See [`Rpc::send_raw_transaction`] for more information.
pub struct SendRawTransaction(pub String);
