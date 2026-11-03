//! Field derivations shared by the `getblock` and `getblockheader` RPCs.

use std::collections::HashMap;

use zebra_chain::{
    amount::NegativeAllowed,
    block::{self, Block, Commitment, Height},
    block_info::BlockInfo,
    orchard,
    parameters::{Network, NetworkUpgrade},
    sapling,
    serialization::BytesInDisplayOrder,
    transaction, transparent,
    value_balance::ValueBalance,
};

use super::{
    types::{get_blockchain_info::GetBlockchainInfoBalance, transaction::TransactionObject},
    BlockchainValuePoolBalances, GetBlockTransaction, GetBlockTrees, IronwoodTrees, OrchardTrees,
    SaplingTrees,
};

#[cfg(test)]
mod tests;

/// Returns the `confirmations` of a block at `depth` below the best chain tip.
pub(crate) fn confirmations(depth: Option<u32>) -> i64 {
    // From <https://zcash.github.io/rpc/getblock.html>
    // TODO: Deduplicate const definition, consider refactoring this to avoid duplicate logic
    const NOT_IN_BEST_CHAIN_CONFIRMATIONS: i64 = -1;

    // Confirmations are one more than the depth.
    // Depth is limited by height, so it will never overflow an i64.
    depth
        .map(|depth| i64::from(depth) + 1)
        .unwrap_or(NOT_IN_BEST_CHAIN_CONFIRMATIONS)
}

/// Returns the `nonce` of `header`, in RPC byte order.
pub(crate) fn nonce(header: &block::Header) -> [u8; 32] {
    let mut nonce = *header.nonce;
    nonce.reverse();
    nonce
}

/// Returns the `finalsaplingroot` of the block at `height`, given the Sapling tree after it.
pub(crate) fn final_sapling_root(
    network: &Network,
    height: Height,
    sapling_tree: &sapling::tree::NoteCommitmentTree,
) -> [u8; 32] {
    let sapling_activation = NetworkUpgrade::Sapling.activation_height(network);
    if sapling_activation.is_some() && height >= sapling_activation.unwrap() {
        let mut root: [u8; 32] = sapling_tree.root().into();
        root.reverse();
        root
    } else {
        [0; 32]
    }
}

/// Returns the `difficulty` of `header`.
pub(crate) fn difficulty(header: &block::Header, network: &Network) -> f64 {
    header.difficulty_threshold.relative_to_network(network)
}

/// Returns the `blockcommitments` of `header` at `height`, given the block's `finalsaplingroot`.
pub(crate) fn block_commitments(
    header: &block::Header,
    network: &Network,
    height: Height,
    final_sapling_root: [u8; 32],
) -> [u8; 32] {
    match header
        .commitment(network, height)
        .expect("Unexpected failure while parsing the blockcommitments field in get_block_header")
    {
        Commitment::PreSaplingReserved(bytes) => bytes,
        Commitment::FinalSaplingRoot(_) => final_sapling_root,
        Commitment::ChainHistoryActivationReserved => [0; 32],
        Commitment::ChainHistoryRoot(root) => root.bytes_in_display_order(),
        Commitment::ChainHistoryBlockTxAuthCommitment(hash) => hash.bytes_in_display_order(),
    }
}

/// Returns the `finalorchardroot` of the block at `height`, given the Orchard tree after it.
pub(crate) fn final_orchard_root(
    network: &Network,
    height: Height,
    orchard_tree: &orchard::tree::NoteCommitmentTree,
) -> Option<[u8; 32]> {
    let nu5_activation = NetworkUpgrade::Nu5.activation_height(network);

    match nu5_activation {
        Some(activation_height) if height >= activation_height => Some(orchard_tree.root().into()),
        _other => None,
    }
}

/// Returns the `trees` of a block, given the sizes of its note commitment trees.
pub(crate) fn trees(
    sapling_tree_size: u64,
    orchard_tree_size: u64,
    ironwood_tree_size: u64,
) -> GetBlockTrees {
    let sapling = SaplingTrees {
        size: sapling_tree_size,
    };

    let orchard = OrchardTrees {
        size: orchard_tree_size,
    };

    let ironwood = IronwoodTrees {
        size: ironwood_tree_size,
    };

    GetBlockTrees {
        sapling,
        orchard,
        ironwood,
    }
}

/// Returns the change in the value pools from the parent block to this block.
pub(crate) fn value_pool_delta(
    block_info: Option<&BlockInfo>,
    prev_block_info: Option<BlockInfo>,
) -> Option<ValueBalance<NegativeAllowed>> {
    block_info.and_then(|d| {
        let value_pools = d.value_pools().constrain::<NegativeAllowed>().ok()?;
        let prev_value_pools = prev_block_info
            .map(|d| d.value_pools().constrain::<NegativeAllowed>())
            .unwrap_or(Ok(ValueBalance::<NegativeAllowed>::zero()))
            .ok()?;
        (value_pools - prev_value_pools).ok()
    })
}

/// Returns the `size` of a block, from its body when it was read, else from its `BlockInfo`.
pub(crate) fn block_size(size: Option<usize>, block_info: Option<&BlockInfo>) -> Option<i64> {
    let size = size.or(block_info.map(|d| d.size() as usize));
    size.map(|size| size as i64)
}

/// Returns the `chainSupply` after a block.
pub(crate) fn chain_supply(block_info: Option<&BlockInfo>) -> Option<GetBlockchainInfoBalance> {
    block_info.map(|d| GetBlockchainInfoBalance::chain_supply(*d.value_pools()))
}

/// Returns the `valuePools` after a block, with their changes in the block.
pub(crate) fn value_pools(
    block_info: Option<BlockInfo>,
    delta: Option<ValueBalance<NegativeAllowed>>,
) -> Option<BlockchainValuePoolBalances> {
    block_info.map(|d| GetBlockchainInfoBalance::value_pools(*d.value_pools(), delta))
}

/// Returns the `tx` of a block as transaction IDs.
pub(crate) fn transaction_ids(tx_ids: &[transaction::Hash]) -> Vec<GetBlockTransaction> {
    tx_ids
        .iter()
        .map(|tx_id| GetBlockTransaction::Hash(*tx_id))
        .collect()
}

/// Returns the `tx` of a block as transaction objects, with each input's `prevout` and the
/// transaction `fee` when `spent_outputs` is given.
pub(crate) fn transaction_objects(
    block: &Block,
    height: Height,
    confirmations: i64,
    network: &Network,
    hash: block::Hash,
    in_active_chain: bool,
    spent_outputs: Option<&HashMap<transparent::OutPoint, transparent::Utxo>>,
) -> Vec<GetBlockTransaction> {
    let block_time = block.header.time;

    let mut transactions = Vec::with_capacity(block.transactions.len());
    for tx in block.transactions.iter() {
        let mut object = TransactionObject::from_transaction(
            tx.clone(),
            Some(height),
            Some(confirmations),
            network,
            Some(block_time),
            Some(hash),
            Some(in_active_chain),
            tx.hash(),
        );

        if let Some(spent_outputs) = spent_outputs {
            object.add_prevouts(tx, spent_outputs, network);
        }

        transactions.push(GetBlockTransaction::Object(Box::new(object)));
    }
    transactions
}
