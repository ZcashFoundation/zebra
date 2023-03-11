//! rkyv RPC method request structs

use bytecheck::CheckBytes;
use rkyv::{Archive, Deserialize, Serialize};

pub use crate::methods::{AddressStrings, GetAddressTxIdsRequest, Rpc};

#[cfg(feature = "getblocktemplate-rpcs")]
pub use super::get_block_template_rpcs::request::*;

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns software information from the RPC server, as a [`GetInfo`](super::response::GetInfo).
///
/// See [`Rpc::get_info`] for more information.
pub struct Info;

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns blockchain state information, as a [`GetBlockChainInfo`].
///
/// See [`Rpc::get_blockchain_info`] for more information.
pub struct BlockChainInfo;

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Total balance of a provided `addresses` in an [`AddressBalance`] instance.
///
/// See [`Rpc::get_address_balance`] for more information.
pub struct AddressBalance(pub AddressStrings);

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Sends the raw bytes of a signed transaction to the local node's mempool, if the transaction is valid.
///
/// See [`Rpc::send_raw_transaction`] for more information.
pub struct SendRawTransaction(pub String);

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns the requested block by hash or height, as a GetBlock JSON string.
/// If the block is not in Zebra's state, returns error code -8.
///
/// See [`Rpc::get_block`] for more information.
pub struct Block {
    /// The hash or height for the block to be returned.
    pub hash_or_height: String,
    /// 0 for hex encoded data, 1 for a json object, and 2 for json object with transaction data.
    pub verbosity: Option<u8>,
}

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns the hash of the current best blockchain tip block.
///
/// See [`Rpc::get_best_block_hash`] for more information.
pub struct BestTipBlockHash;

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns all transaction ids in the memory pool.
///
/// See [`Rpc::get_raw_mempool`] for more information.
pub struct RawMempool;

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns the raw transaction data.
///
/// See [`Rpc::get_raw_transaction`] for more information.
pub struct RawTransaction {
    /// - `txid`: (string, required) The transaction ID of the transaction to be returned.
    pub tx_id_hex: String,
    /// - `verbose`: (numeric, optional, default=0) If 0, return a string of hex-encoded data, otherwise return a JSON object.
    pub verbose: u8,
}

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns information about the given block's Sapling & Orchard tree state.
///
/// See [`Rpc::z_get_treestate`] for more information.
pub struct ZTreestate(pub String);

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns all unspent outputs for a list of addresses.
///
/// See [`Rpc::get_address_utxos`] for more information.
pub struct AddressUtxos(pub AddressStrings);
