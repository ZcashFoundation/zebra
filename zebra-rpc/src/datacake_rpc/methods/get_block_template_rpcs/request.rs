//! rkyv getblocktemplate RPC method request structs

use bytecheck::CheckBytes;
use rkyv::{Archive, Deserialize, Serialize};

use crate::methods::get_block_template_rpcs::{get_block_template, types::hex_data::HexData};

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns the height of the most recent block in the best valid block chain.
///
/// See [`Rpc::get_block_count`] for more information.
pub struct BlockCount;

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns the hash of the block of a given height if the index argument correspond
/// to a block in the best chain.
///
/// See [`Rpc::get_block_hash`] for more information.
pub struct BlockHash(pub i32);

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns a block template for mining new Zcash blocks.
///
/// See [`Rpc::get_block_template`] for more information.
pub struct BlockTemplate(pub get_block_template::JsonParameters);

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Submits block to the node to be validated and committed.
///
/// See [`Rpc::submit_block`] for more information.
pub struct SubmitBlock(pub HexData);

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns mining-related information.
///
/// See [`Rpc::get_mining_info`] for more information.
pub struct MiningInfo;

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns the estimated network solutions per second based on the last
/// `num_blocks` before `height`.
///
/// See [`Rpc::get_network_sol_ps`] for more information.
pub struct NetworkSolPs {
    /// Number of blocks to check when estimating network solution rate.
    pub num_blocks: Option<usize>,

    /// Block height of solution rate estimate.
    pub height: Option<i32>,
}

#[repr(C)]
#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Returns data about each connected network node.
///
/// See [`Rpc::getpeerinfo`] for more information.
pub struct PeerInfo;
