//! The state reads a `getblock` call needs, and the field derivations shared by the `getblock`
//! and `getblockheader` RPCs.

use std::collections::{BTreeSet, HashMap};

use jsonrpsee_types::{ErrorObject, ErrorObjectOwned};
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
use zebra_state::{BlockQueryFields, QueriedBlock};

use crate::server::error::{LegacyCode, OkOrError};

use super::{
    types::{
        get_block::{GetBlockField, GetBlockQuery},
        get_blockchain_info::GetBlockchainInfoBalance,
        transaction::TransactionObject,
    },
    BlockObject, BlockchainValuePoolBalances, GetBlockTransaction, GetBlockTrees, IronwoodTrees,
    OrchardTrees, SaplingTrees,
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

/// The shape of the `tx` key of a `getblock` object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxShape {
    /// Transaction IDs, as verbosity 1 and the `tx` field return them.
    Ids,
    /// Transaction objects, as verbosity 2 and the `txdetails` field return them.
    Objects,
    /// Transaction objects with each input's `prevout` and the transaction `fee`, as verbosity 3
    /// and the `txprevouts` field return them.
    ObjectsWithPrevouts,
}

impl TxShape {
    /// Returns the shape `field` selects, if it is one of the `tx` shapes.
    fn of(field: GetBlockField) -> Option<Self> {
        match field {
            GetBlockField::Tx => Some(Self::Ids),
            GetBlockField::TxDetails => Some(Self::Objects),
            GetBlockField::TxPrevouts => Some(Self::ObjectsWithPrevouts),
            _ => None,
        }
    }

    /// Returns the name of the field that selects this shape.
    fn field_name(self) -> &'static str {
        match self {
            Self::Ids => "tx",
            Self::Objects => "txdetails",
            Self::ObjectsWithPrevouts => "txprevouts",
        }
    }
}

/// The keys verbosity 1 returns. Verbosity 2 and 3 return the same keys with `tx` in another
/// shape.
pub(crate) const VERBOSITY_1_FIELDS: [GetBlockField; 21] = [
    GetBlockField::Hash,
    GetBlockField::Confirmations,
    GetBlockField::Size,
    GetBlockField::Height,
    GetBlockField::Version,
    GetBlockField::MerkleRoot,
    GetBlockField::BlockCommitments,
    GetBlockField::FinalSaplingRoot,
    GetBlockField::FinalOrchardRoot,
    GetBlockField::TxCount,
    GetBlockField::Tx,
    GetBlockField::Time,
    GetBlockField::Nonce,
    GetBlockField::Solution,
    GetBlockField::Bits,
    GetBlockField::Difficulty,
    GetBlockField::ChainSupply,
    GetBlockField::ValuePools,
    GetBlockField::Trees,
    GetBlockField::PreviousBlockHash,
    GetBlockField::NextBlockHash,
];

/// The keys a `getblock` call returns, and the state reads they need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GetBlockPlan {
    /// Which JSON keys to fill (verbosity presets or the requested set).
    pub(crate) fields: BTreeSet<GetBlockField>,
    /// The state reads those keys need, after subsumption.
    pub(crate) state_fields: BlockQueryFields,
    /// Which shape `tx` takes: ids (verbosity 1), objects (2), objects with prevouts and fees (3).
    pub(crate) tx_shape: Option<TxShape>,
}

/// The reads that return the deserialized block: `TRANSACTIONS` and the reads that imply it.
const TRANSACTIONS_READS: BlockQueryFields = BlockQueryFields::TRANSACTIONS
    .union(BlockQueryFields::SPENT_OUTPUTS)
    .union(BlockQueryFields::AUTH_DATA_ROOT);

impl GetBlockPlan {
    /// Returns the plan that fills `fields`, which hold at most one `tx` shape.
    pub(crate) fn new(fields: BTreeSet<GetBlockField>) -> Self {
        let mut state_fields = fields
            .iter()
            .fold(BlockQueryFields::empty(), |reads, field| {
                reads | state_fields(*field)
            });

        // `nTx` is the length of the transaction IDs or of the transactions when either is read.
        if state_fields.intersects(BlockQueryFields::TRANSACTION_IDS | TRANSACTIONS_READS) {
            state_fields.remove(BlockQueryFields::TRANSACTION_COUNT);
        }

        // A body read returns the block size, so `BlockInfo` is only needed for the value pools.
        if state_fields.intersects(BlockQueryFields::RAW_BLOCK | TRANSACTIONS_READS)
            && !fields.contains(&GetBlockField::ChainSupply)
            && !fields.contains(&GetBlockField::ValuePools)
        {
            state_fields.remove(BlockQueryFields::BLOCK_INFO);
        }

        let tx_shape = fields.iter().find_map(|field| TxShape::of(*field));

        Self {
            fields,
            state_fields,
            tx_shape,
        }
    }

    /// Returns whether this plan reads a block body or spent transaction bodies.
    pub(crate) fn reads_body(&self) -> bool {
        self.state_fields
            .intersects(BlockQueryFields::RAW_BLOCK | TRANSACTIONS_READS)
    }
}

/// Returns the state reads `field` needs, before a plan drops the reads other reads subsume.
fn state_fields(field: GetBlockField) -> BlockQueryFields {
    use GetBlockField::*;

    match field {
        Hash | Height => BlockQueryFields::empty(),
        Confirmations => BlockQueryFields::DEPTH,
        Size | ChainSupply => BlockQueryFields::BLOCK_INFO,
        ValuePools => BlockQueryFields::BLOCK_INFO | BlockQueryFields::PREVIOUS_BLOCK_INFO,
        Version | Time | Nonce | Solution | Bits | PreviousBlockHash | MerkleRoot | Difficulty => {
            BlockQueryFields::HEADER
        }
        // Before Heartwood the commitment is the Sapling tree root, not the header bytes, which
        // the state does not check against the tree.
        BlockCommitments => BlockQueryFields::HEADER | BlockQueryFields::SAPLING_TREE,
        AuthDataRoot => BlockQueryFields::AUTH_DATA_ROOT,
        FinalSaplingRoot => BlockQueryFields::SAPLING_TREE,
        FinalOrchardRoot => BlockQueryFields::ORCHARD_TREE,
        TxCount => BlockQueryFields::TRANSACTION_COUNT,
        Tx => BlockQueryFields::TRANSACTION_IDS,
        TxDetails => BlockQueryFields::TRANSACTIONS | BlockQueryFields::DEPTH,
        TxPrevouts => {
            BlockQueryFields::TRANSACTIONS
                | BlockQueryFields::SPENT_OUTPUTS
                | BlockQueryFields::DEPTH
        }
        Trees => {
            BlockQueryFields::SAPLING_TREE
                | BlockQueryFields::ORCHARD_TREE
                | BlockQueryFields::IRONWOOD_TREE
        }
        NextBlockHash => BlockQueryFields::NEXT_BLOCK_HASH,
        Hex => BlockQueryFields::RAW_BLOCK,
    }
}

impl TryFrom<&GetBlockQuery> for GetBlockPlan {
    type Error = ErrorObjectOwned;

    fn try_from(query: &GetBlockQuery) -> Result<Self, Self::Error> {
        let fields = match query {
            GetBlockQuery::Verbosity(0) => BTreeSet::from([GetBlockField::Hex]),
            GetBlockQuery::Verbosity(verbosity @ 1..=3) => {
                let tx = match verbosity {
                    1 => GetBlockField::Tx,
                    2 => GetBlockField::TxDetails,
                    _ => GetBlockField::TxPrevouts,
                };
                VERBOSITY_1_FIELDS
                    .into_iter()
                    .filter(|field| *field != GetBlockField::Tx)
                    .chain([tx])
                    .collect()
            }
            GetBlockQuery::Verbosity(_) => {
                return Err(invalid_parameter("invalid verbosity value"))
            }
            GetBlockQuery::Fields(fields) => {
                if fields.is_empty() {
                    return Err(invalid_parameter("empty field list"));
                }

                // The set iterates in field order, so the names are in a deterministic order.
                let shapes: Vec<_> = fields
                    .iter()
                    .filter_map(|field| TxShape::of(*field))
                    .map(TxShape::field_name)
                    .collect();
                if shapes.len() > 1 {
                    return Err(invalid_parameter(format!(
                        "conflicting fields: {}",
                        shapes.join(", ")
                    )));
                }

                fields.clone()
            }
        };

        Ok(Self::new(fields))
    }
}

/// Returns an invalid parameter error with `message`.
fn invalid_parameter(message: impl ToString) -> ErrorObjectOwned {
    ErrorObject::owned(
        i32::from(LegacyCode::InvalidParameter),
        message.to_string(),
        None::<()>,
    )
}

/// Returns the error for a block that is not in the best chain: code `-5` if it was requested by
/// hash, or `-8` if it was requested by height.
pub(crate) fn not_found(
    hash_or_height: &zebra_state::HashOrHeight,
    message: &str,
) -> ErrorObjectOwned {
    let code = if hash_or_height.hash().is_some() {
        LegacyCode::InvalidAddressOrKey
    } else {
        LegacyCode::InvalidParameter
    };

    ErrorObject::owned(i32::from(code), message, None::<()>)
}

/// Returns `part` if `read` is set, failing with `message` if the state read returned no part.
///
/// The state returns errors for rows a primary database is missing, so a missing part comes from a
/// torn secondary view.
fn required<T>(read: bool, part: Option<T>, message: &str) -> Result<Option<T>, ErrorObjectOwned> {
    if read {
        part.ok_or_misc_error(message).map(Some)
    } else {
        Ok(None)
    }
}

/// Returns the `getblock` object with the keys in `plan`, from the parts of `block` that the
/// plan's state request read.
///
/// With `require_block_info`, a requested `size`, `chainSupply` or `valuePools` whose `BlockInfo`
/// row the plan read but the state did not return fails with `missing block info`. Without it
/// those keys are omitted, as the verbosity levels did for databases that have no `BlockInfo` rows
/// yet.
pub(crate) fn block_object(
    block: QueriedBlock,
    plan: &GetBlockPlan,
    network: &Network,
    require_block_info: bool,
) -> Result<BlockObject, ErrorObjectOwned> {
    use GetBlockField::*;

    let has = |field| plan.fields.contains(&field);
    let reads = |part| plan.state_fields.contains(part);

    let QueriedBlock {
        hash,
        height,
        header,
        depth,
        next_block_hash,
        transaction_count,
        transaction_ids: tx_ids,
        block_info,
        previous_block_info,
        sapling_tree,
        orchard_tree,
        ironwood_tree,
        raw_block,
        block: body,
        size: body_size,
        spent_outputs,
        auth_data_root,
    } = block;

    // Missing parts are checked in the order the handler read them when each was its own request,
    // so a torn view reports the same error as it did then.
    let header = required(reads(BlockQueryFields::HEADER), header, "Block not found")?;
    let sapling_tree = required(
        reads(BlockQueryFields::SAPLING_TREE),
        sapling_tree,
        "missing Sapling tree",
    )?;

    let tx = match plan.tx_shape {
        None => None,
        Some(TxShape::Ids) => Some(transaction_ids(
            &tx_ids.ok_or_misc_error("block not found")?,
        )),
        Some(shape) => {
            let body = body.as_ref().ok_or_misc_error("Block not found")?;
            let tx_confirmations = confirmations(depth);
            // A spent output that the state could not read leaves its input without a `prevout`
            // and its transaction without a `fee`.
            let spent_outputs =
                (shape == TxShape::ObjectsWithPrevouts).then(|| spent_outputs.unwrap_or_default());
            Some(transaction_objects(
                body,
                height,
                tx_confirmations,
                network,
                hash,
                tx_confirmations >= 0,
                spent_outputs.as_ref(),
            ))
        }
    };

    let orchard_tree = required(
        reads(BlockQueryFields::ORCHARD_TREE),
        orchard_tree,
        "missing Orchard tree",
    )?;
    let ironwood_tree = required(
        reads(BlockQueryFields::IRONWOOD_TREE),
        ironwood_tree,
        "missing Ironwood tree",
    )?;

    // The plan reads `BlockInfo` only for the keys that need it. When the body supplies `size`,
    // a missing body is reported by the body's own check below.
    if require_block_info && reads(BlockQueryFields::BLOCK_INFO) && block_info.is_none() {
        return Err(ErrorObject::owned(
            i32::from(LegacyCode::Misc),
            "missing block info",
            None::<()>,
        ));
    }

    let raw_block = required(has(Hex), raw_block, "Block not found")?;
    let auth_data_root = required(has(AuthDataRoot), auth_data_root, "Block not found")?;

    let n_tx = if has(TxCount) {
        Some(match (&tx, &body) {
            (Some(tx), _) => tx.len(),
            (None, Some(body)) => body.transactions.len(),
            (None, None) => transaction_count.ok_or_misc_error("block not found")?,
        })
    } else {
        None
    };

    let sapling_root = sapling_tree
        .as_ref()
        .map(|tree| final_sapling_root(network, height, tree));
    let header_field = |field| header.as_ref().filter(|_| has(field));

    let block_trees = sapling_tree
        .as_ref()
        .zip(orchard_tree.as_ref())
        .zip(ironwood_tree.as_ref())
        .filter(|_| has(Trees))
        .map(|((sapling, orchard), ironwood)| {
            trees(sapling.count(), orchard.count(), ironwood.count())
        });

    let size = if has(Size) {
        block_size(body_size, block_info.as_ref())
    } else {
        None
    };
    let chain_supply = if has(ChainSupply) {
        chain_supply(block_info.as_ref())
    } else {
        None
    };
    let value_pools = if has(ValuePools) {
        let delta = value_pool_delta(block_info.as_ref(), previous_block_info);
        value_pools(block_info, delta)
    } else {
        None
    };

    Ok(BlockObject {
        hash,
        confirmations: has(Confirmations).then(|| confirmations(depth)),
        size,
        height: has(Height).then_some(height),
        version: header_field(Version).map(|header| header.version),
        merkle_root: header_field(MerkleRoot).map(|header| header.merkle_root),
        block_commitments: header_field(BlockCommitments)
            .zip(sapling_root)
            .map(|(header, sapling_root)| block_commitments(header, network, height, sapling_root)),
        auth_data_root,
        final_sapling_root: sapling_root.filter(|_| has(FinalSaplingRoot)),
        final_orchard_root: orchard_tree
            .as_ref()
            .filter(|_| has(FinalOrchardRoot))
            .and_then(|tree| final_orchard_root(network, height, tree)),
        n_tx,
        tx,
        time: header_field(Time).map(|header| header.time.timestamp()),
        nonce: header_field(Nonce).map(|header| nonce(header)),
        solution: header_field(Solution).map(|header| header.solution),
        bits: header_field(Bits).map(|header| header.difficulty_threshold),
        difficulty: header_field(Difficulty).map(|header| difficulty(header, network)),
        chain_supply,
        value_pools,
        trees: block_trees,
        previous_block_hash: header_field(PreviousBlockHash)
            .map(|header| header.previous_block_hash),
        next_block_hash: next_block_hash.filter(|_| has(NextBlockHash)),
        hex: raw_block,
    })
}
