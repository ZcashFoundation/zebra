//! The `GetBlockTempate` type is the output of the `getblocktemplate` RPC method in the
//! default 'template' mode. See [`ProposalResponse`] for the output in 'proposal' mode.

use std::fmt;

use zebra_chain::{
    amount,
    block::{ChainHistoryBlockTxAuthCommitmentHash, MAX_BLOCK_BYTES, ZCASH_BLOCK_VERSION},
    parameters::Network,
    serialization::DateTime32,
    transaction::VerifiedUnminedTx,
    transparent,
    work::difficulty::{CompactDifficulty, ExpandedDifficulty},
};
use zebra_consensus::MAX_BLOCK_SIGOPS;
use zebra_state::GetBlockTemplateChainInfo;

use crate::methods::{
    get_block_template_rpcs::{
        constants::{
            GET_BLOCK_TEMPLATE_CAPABILITIES_FIELD, GET_BLOCK_TEMPLATE_MUTABLE_FIELD,
            GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD,
        },
        get_block_template::generate_coinbase_and_roots,
        types::{
            default_roots::DefaultRoots, long_poll::LongPollId, transaction::TransactionTemplate,
        },
    },
    GetBlockHash,
};

pub mod parameters;
pub mod proposal;

pub use parameters::{GetBlockTemplateCapability, GetBlockTemplateRequestMode, JsonParameters};
pub use proposal::{proposal_block_from_template, ProposalResponse};

/// An alias to indicate that a usize value represents the depth of in-block dependencies of a transaction.
///
/// See the `dependencies_depth()` function in [`zip317`](super::super::zip317) for more details.
pub type InBlockTxDependenciesDepth = usize;

/// A serialized `getblocktemplate` RPC response in template mode.
#[derive(Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetBlockTemplate {
    /// The getblocktemplate RPC capabilities supported by Zebra.
    ///
    /// At the moment, Zebra does not support any of the extra capabilities from the specification:
    /// - `proposal`: <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
    /// - `longpoll`: <https://en.bitcoin.it/wiki/BIP_0022#Optional:_Long_Polling>
    /// - `serverlist`: <https://en.bitcoin.it/wiki/BIP_0023#Logical_Services>
    ///
    /// By the above, Zebra will always return an empty vector here.
    pub capabilities: Vec<String>,

    /// The version of the block format.
    /// Always 4 for new Zcash blocks.
    pub version: u32,

    /// The hash of the previous block.
    #[serde(rename = "previousblockhash")]
    pub previous_block_hash: GetBlockHash,

    /// The block commitment for the new block's header.
    ///
    /// Same as [`DefaultRoots.block_commitments_hash`], see that field for details.
    #[serde(rename = "blockcommitmentshash")]
    #[serde(with = "hex")]
    pub block_commitments_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// Legacy backwards-compatibility header root field.
    ///
    /// Same as [`DefaultRoots.block_commitments_hash`], see that field for details.
    #[serde(rename = "lightclientroothash")]
    #[serde(with = "hex")]
    pub light_client_root_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// Legacy backwards-compatibility header root field.
    ///
    /// Same as [`DefaultRoots.block_commitments_hash`], see that field for details.
    #[serde(rename = "finalsaplingroothash")]
    #[serde(with = "hex")]
    pub final_sapling_root_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// The block header roots for [`GetBlockTemplate.transactions`].
    ///
    /// If the transactions in the block template are modified, these roots must be recalculated
    /// [according to the specification](https://zcash.github.io/rpc/getblocktemplate.html).
    #[serde(rename = "defaultroots")]
    pub default_roots: DefaultRoots,

    /// The non-coinbase transactions selected for this block template.
    pub transactions: Vec<TransactionTemplate<amount::NonNegative>>,

    /// The coinbase transaction generated from `transactions` and `height`.
    #[serde(rename = "coinbasetxn")]
    pub coinbase_txn: TransactionTemplate<amount::NegativeOrZero>,

    /// An ID that represents the chain tip and mempool contents for this template.
    #[serde(rename = "longpollid")]
    pub long_poll_id: LongPollId,

    /// The expected difficulty for the new block displayed in expanded form.
    #[serde(with = "hex")]
    pub target: ExpandedDifficulty,

    /// > For each block other than the genesis block, nTime MUST be strictly greater than
    /// > the median-time-past of that block.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#blockheader>
    #[serde(rename = "mintime")]
    pub min_time: DateTime32,

    /// Hardcoded list of block fields the miner is allowed to change.
    pub mutable: Vec<String>,

    /// A range of valid nonces that goes from `u32::MIN` to `u32::MAX`.
    #[serde(rename = "noncerange")]
    pub nonce_range: String,

    /// Max legacy signature operations in the block.
    #[serde(rename = "sigoplimit")]
    pub sigop_limit: u64,

    /// Max block size in bytes
    #[serde(rename = "sizelimit")]
    pub size_limit: u64,

    /// > the current time as seen by the server (recommended for block time).
    /// > note this is not necessarily the system clock, and must fall within the mintime/maxtime rules
    ///
    /// <https://en.bitcoin.it/wiki/BIP_0022#Block_Template_Request>
    #[serde(rename = "curtime")]
    pub cur_time: DateTime32,

    /// The expected difficulty for the new block displayed in compact form.
    #[serde(with = "hex")]
    pub bits: CompactDifficulty,

    /// The height of the next block in the best chain.
    // Optional TODO: use Height type, but check that deserialized heights are within Height::MAX
    pub height: u32,

    /// > the maximum time allowed
    ///
    /// <https://en.bitcoin.it/wiki/BIP_0023#Mutations>
    ///
    /// Zebra adjusts the minimum and current times for testnet minimum difficulty blocks,
    /// so we need to tell miners what the maximum valid time is.
    ///
    /// This field is not in `zcashd` or the Zcash RPC reference yet.
    ///
    /// Currently, some miners just use `min_time` or `cur_time`. Others calculate `max_time` from the
    /// fixed 90 minute consensus rule, or a smaller fixed interval (like 1000s).
    /// Some miners don't check the maximum time. This can cause invalid blocks after network downtime,
    /// a significant drop in the hash rate, or after the testnet minimum difficulty interval.
    #[serde(rename = "maxtime")]
    pub max_time: DateTime32,

    /// > only relevant for long poll responses:
    /// > indicates if work received prior to this response remains potentially valid (default)
    /// > and should have its shares submitted;
    /// > if false, the miner may wish to discard its share queue
    ///
    /// <https://en.bitcoin.it/wiki/BIP_0022#Optional:_Long_Polling>
    ///
    /// This field is not in `zcashd` or the Zcash RPC reference yet.
    ///
    /// In Zebra, `submit_old` is `false` when the tip block changed or max time is reached,
    /// and `true` if only the mempool transactions have changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    #[serde(rename = "submitold")]
    pub submit_old: Option<bool>,
}

impl fmt::Debug for GetBlockTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A block with a lot of transactions can be extremely long in logs.
        let mut transactions_truncated = self.transactions.clone();
        if self.transactions.len() > 4 {
            // Remove transaction 3 onwards, but leave the last transaction
            let end = self.transactions.len() - 2;
            transactions_truncated.splice(3..=end, Vec::new());
        }

        f.debug_struct("GetBlockTemplate")
            .field("capabilities", &self.capabilities)
            .field("version", &self.version)
            .field("previous_block_hash", &self.previous_block_hash)
            .field("block_commitments_hash", &self.block_commitments_hash)
            .field("light_client_root_hash", &self.light_client_root_hash)
            .field("final_sapling_root_hash", &self.final_sapling_root_hash)
            .field("default_roots", &self.default_roots)
            .field("transaction_count", &self.transactions.len())
            .field("transactions", &transactions_truncated)
            .field("coinbase_txn", &self.coinbase_txn)
            .field("long_poll_id", &self.long_poll_id)
            .field("target", &self.target)
            .field("min_time", &self.min_time)
            .field("mutable", &self.mutable)
            .field("nonce_range", &self.nonce_range)
            .field("sigop_limit", &self.sigop_limit)
            .field("size_limit", &self.size_limit)
            .field("cur_time", &self.cur_time)
            .field("bits", &self.bits)
            .field("height", &self.height)
            .field("max_time", &self.max_time)
            .field("submit_old", &self.submit_old)
            .finish()
    }
}

impl GetBlockTemplate {
    /// Returns a `Vec` of capabilities supported by the `getblocktemplate` RPC
    pub fn capabilities() -> Vec<String> {
        GET_BLOCK_TEMPLATE_CAPABILITIES_FIELD
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// Returns a new [`GetBlockTemplate`] struct, based on the supplied arguments and defaults.
    ///
    /// The result of this method only depends on the supplied arguments and constants.
    ///
    /// If `like_zcashd` is true, try to match the coinbase transactions generated by `zcashd`
    /// in the `getblocktemplate` RPC.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        network: &Network,
        miner_address: &transparent::Address,
        chain_tip_and_local_time: &GetBlockTemplateChainInfo,
        long_poll_id: LongPollId,
        #[cfg(not(test))] mempool_txs: Vec<VerifiedUnminedTx>,
        #[cfg(test)] mempool_txs: Vec<(InBlockTxDependenciesDepth, VerifiedUnminedTx)>,
        submit_old: Option<bool>,
        like_zcashd: bool,
        extra_coinbase_data: Vec<u8>,
    ) -> Self {
        // Calculate the next block height.
        let next_block_height =
            (chain_tip_and_local_time.tip_height + 1).expect("tip is far below Height::MAX");

        // Convert transactions into TransactionTemplates
        #[cfg(not(test))]
        let (mempool_tx_templates, mempool_txs): (Vec<_>, Vec<_>) =
            mempool_txs.into_iter().map(|tx| ((&tx).into(), tx)).unzip();

        // Transaction selection returns transactions in an arbitrary order,
        // but Zebra's snapshot tests expect the same order every time.
        //
        // # Correctness
        //
        // Transactions that spend outputs created in the same block must appear
        // after the transactions that create those outputs.
        #[cfg(test)]
        let (mempool_tx_templates, mempool_txs): (Vec<_>, Vec<_>) = {
            let mut mempool_txs_with_templates: Vec<(
                InBlockTxDependenciesDepth,
                TransactionTemplate<amount::NonNegative>,
                VerifiedUnminedTx,
            )> = mempool_txs
                .into_iter()
                .map(|(min_tx_index, tx)| (min_tx_index, (&tx).into(), tx))
                .collect();

            if like_zcashd {
                // Sort in serialized data order, excluding the length byte.
                // `zcashd` sometimes seems to do this, but other times the order is arbitrary.
                mempool_txs_with_templates.sort_by_key(|(min_tx_index, tx_template, _tx)| {
                    (*min_tx_index, tx_template.data.clone())
                });
            } else {
                // Sort by hash, this is faster.
                mempool_txs_with_templates.sort_by_key(|(min_tx_index, tx_template, _tx)| {
                    (*min_tx_index, tx_template.hash.bytes_in_display_order())
                });
            }
            mempool_txs_with_templates
                .into_iter()
                .map(|(_, template, tx)| (template, tx))
                .unzip()
        };

        // Generate the coinbase transaction and default roots
        //
        // TODO: move expensive root, hash, and tree cryptography to a rayon thread?
        let (coinbase_txn, default_roots) = generate_coinbase_and_roots(
            network,
            next_block_height,
            miner_address,
            &mempool_txs,
            chain_tip_and_local_time.chain_history_root,
            like_zcashd,
            extra_coinbase_data,
        );

        // Convert difficulty
        let target = chain_tip_and_local_time
            .expected_difficulty
            .to_expanded()
            .expect("state always returns a valid difficulty value");

        // Convert default values
        let capabilities: Vec<String> = Self::capabilities();
        let mutable: Vec<String> = GET_BLOCK_TEMPLATE_MUTABLE_FIELD
            .iter()
            .map(ToString::to_string)
            .collect();

        tracing::debug!(
            selected_txs = ?mempool_txs
                .iter()
                .map(|tx| (tx.transaction.id.mined_id(), tx.unpaid_actions))
                .collect::<Vec<_>>(),
            "creating template ... "
        );

        GetBlockTemplate {
            capabilities,

            version: ZCASH_BLOCK_VERSION,

            previous_block_hash: GetBlockHash(chain_tip_and_local_time.tip_hash),
            block_commitments_hash: default_roots.block_commitments_hash,
            light_client_root_hash: default_roots.block_commitments_hash,
            final_sapling_root_hash: default_roots.block_commitments_hash,
            default_roots,

            transactions: mempool_tx_templates,

            coinbase_txn,

            long_poll_id,

            target,

            min_time: chain_tip_and_local_time.min_time,

            mutable,

            nonce_range: GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD.to_string(),

            sigop_limit: MAX_BLOCK_SIGOPS,

            size_limit: MAX_BLOCK_BYTES,

            cur_time: chain_tip_and_local_time.cur_time,

            bits: chain_tip_and_local_time.expected_difficulty,

            height: next_block_height.0,

            max_time: chain_tip_and_local_time.max_time,

            submit_old,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
/// A `getblocktemplate` RPC response.
pub enum Response {
    /// `getblocktemplate` RPC request in template mode.
    TemplateMode(Box<GetBlockTemplate>),

    /// `getblocktemplate` RPC request in proposal mode.
    ProposalMode(ProposalResponse),
}

impl Response {
    /// Returns the inner template, if the response is in template mode.
    pub fn try_into_template(self) -> Option<GetBlockTemplate> {
        match self {
            Response::TemplateMode(template) => Some(*template),
            Response::ProposalMode(_) => None,
        }
    }

    /// Returns the inner proposal, if the response is in proposal mode.
    pub fn try_into_proposal(self) -> Option<ProposalResponse> {
        match self {
            Response::TemplateMode(_) => None,
            Response::ProposalMode(proposal) => Some(proposal),
        }
    }
}
