//! rkyv RPC method responses

use bytecheck::CheckBytes;
use rkyv::{Archive, Deserialize, Serialize};

use zebra_chain::{
    block::{self, Height},
    orchard,
    parameters::ConsensusBranchId,
    sapling,
};

pub use crate::methods::{
    AddressBalance, ConsensusBranchIdHex, GetAddressUtxos, GetBlock, GetBlockHash, GetInfo,
    GetRawTransaction, NetworkUpgradeInfo, SentTransactionHash, TipConsensusBranch,
};

#[cfg(feature = "getblocktemplate-rpcs")]
pub use super::get_block_template_rpcs::response::*;

/// An IndexMap entry, see 'upgrades' field [`GetBlockChainInfo`](crate::methods::GetBlockChainInfo)

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
pub struct NetworkUpgradeInfoEntry(pub ConsensusBranchId, pub NetworkUpgradeInfo);

#[derive(Serialize, Deserialize, Archive, PartialEq, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
/// Blockchain state information, as a [`GetBlockChainInfo`].
///
/// See [`Rpc::get_blockchain_info`] for more information.
pub struct GetBlockChainInfo {
    /// Current network name as defined in BIP70 (main, test, regtest)
    chain: String,

    /// The current number of blocks processed in the server, numeric
    blocks: Height,

    /// The hash of the currently best block, in big-endian order, hex-encoded
    best_block_hash: block::Hash,

    /// If syncing, the estimated height of the chain, else the current best height, numeric.
    ///
    /// In Zebra, this is always the height estimate, so it might be a little inaccurate.
    estimated_height: Height,

    /// Status of network upgrades
    upgrades: Vec<NetworkUpgradeInfoEntry>,

    /// Branch IDs of the current consensus rules
    consensus_chain_tip: ConsensusBranchId,

    /// Branch IDs of the upcoming consensus rules
    consensus_next_block: ConsensusBranchId,
}

impl From<crate::methods::GetBlockChainInfo> for GetBlockChainInfo {
    fn from(
        crate::methods::GetBlockChainInfo {
            chain,
            blocks,
            best_block_hash,
            estimated_height,
            upgrades,
            consensus:
                TipConsensusBranch {
                    chain_tip: ConsensusBranchIdHex(consensus_chain_tip),
                    next_block: ConsensusBranchIdHex(consensus_next_block),
                },
        }: crate::methods::GetBlockChainInfo,
    ) -> Self {
        let upgrades = upgrades
            .into_iter()
            .map(|(ConsensusBranchIdHex(k), v)| NetworkUpgradeInfoEntry(k, v))
            .collect();

        Self {
            chain,
            blocks,
            best_block_hash,
            estimated_height,
            upgrades,
            consensus_chain_tip,
            consensus_next_block,
        }
    }
}

/// Contains the Sapling & Orchard note commitment trees, and their
/// corresponding [`block::Hash`], [`Height`], and block time.

#[derive(Serialize, Deserialize, Archive, PartialEq, Clone, Debug)]
#[archive_attr(derive(CheckBytes, PartialEq, Debug))]
pub struct GetTreestate {
    /// The block hash corresponding to the treestate.
    hash: block::Hash,

    /// The block height corresponding to the treestate, numeric.
    height: Height,

    /// Unix time when the block corresponding to the treestate was mined,
    /// numeric.
    ///
    /// UTC seconds since the Unix 1970-01-01 epoch.
    time: u32,

    /// A treestate containing a Sapling note commitment tree.
    sapling_commitment_tree: sapling::tree::SerializedTree,

    /// A treestate containing an Orchard note commitment tree.
    orchard_commitment_tree: orchard::tree::SerializedTree,
}

impl From<crate::methods::GetTreestate> for GetTreestate {
    fn from(
        crate::methods::GetTreestate {
            hash,
            height,
            time,
            sapling,
            orchard,
        }: crate::methods::GetTreestate,
    ) -> Self {
        Self {
            hash,
            height,
            time,
            sapling_commitment_tree: sapling.commitments.final_state,
            orchard_commitment_tree: orchard.commitments.final_state,
        }
    }
}
