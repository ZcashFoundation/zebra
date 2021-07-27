//! Consensus rule checks for the finalized state.

use std::collections::{HashMap, HashSet};

use zebra_chain::{
    block,
    transparent::{self, CoinbaseSpendRestriction::*},
};

use crate::{
    constants::MIN_TRANSPARENT_COINBASE_MATURITY,
    service::finalized_state::FinalizedState,
    PreparedBlock,
    ValidateContextError::{
        self, DuplicateTransparentSpend, EarlyTransparentSpend, ImmatureTransparentCoinbaseSpend,
        MissingTransparentOutput, UnshieldedTransparentCoinbaseSpend,
    },
};

/// Reject invalid spends of transparent outputs.
///
/// Double-spends:
/// - duplicate spends that are both in this block,
/// - spends of an output that was spent by a previous block,
///
/// Missing spends:
/// - spends of an output that hasn't been created yet,
///   (in linear chain and transaction order),
/// - spends of UTXOs that were never created in this chain,
///
/// Invalid spends:
/// - spends of an immature transparent coinbase output,
/// - unshielded spends of a transparent coinbase output.
pub fn transparent_spend(
    prepared: &PreparedBlock,
    non_finalized_chain_unspent_utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
    non_finalized_chain_spent_utxos: &HashSet<transparent::OutPoint>,
    finalized_state: &FinalizedState,
) -> Result<(), ValidateContextError> {
    let mut block_spends = HashSet::new();

    for (spend_tx_index_in_block, transaction) in prepared.block.transactions.iter().enumerate() {
        let spends = transaction.inputs().iter().filter_map(|input| match input {
            transparent::Input::PrevOut { outpoint, .. } => Some(outpoint),
            // Coinbase inputs represent new coins,
            // so there are no UTXOs to mark as spent.
            transparent::Input::Coinbase { .. } => None,
        });

        for spend in spends {
            // see `transparent_spend_chain_order` for the consensus rule
            if !block_spends.insert(*spend) {
                // reject in-block duplicate spends
                return Err(DuplicateTransparentSpend {
                    outpoint: *spend,
                    location: "the same block",
                });
            }

            let utxo = transparent_spend_chain_order(
                *spend,
                spend_tx_index_in_block,
                &prepared.new_outputs,
                non_finalized_chain_unspent_utxos,
                non_finalized_chain_spent_utxos,
                finalized_state,
            )?;

            // The state service returns UTXOs from pending blocks,
            // which can be rejected by later contextual checks.
            // This is a particular issue for v5 transactions,
            // because their authorizing data is only bound to the block data
            // during contextual validation (#2336).
            //
            // We don't want to use UTXOs from invalid pending blocks,
            // so we check transparent coinbase maturity and shielding
            // using known valid UTXOs during non-finalized chain validation.

            let spend_restriction = transaction.coinbase_spend_restriction(prepared.height);
            transparent_coinbase_spend(*spend, spend_restriction, utxo)?;
        }
    }

    Ok(())
}

/// Check that transparent spends occur in chain order.
///
/// Because we are in the non-finalized state, we need to check spends within the same block,
/// spent non-finalized UTXOs, and unspent non-finalized and finalized UTXOs.
///
/// "Any input within this block can spend an output which also appears in this block
/// (assuming the spend is otherwise valid).
/// However, the TXID corresponding to the output must be placed at some point
/// before the TXID corresponding to the input.
/// This ensures that any program parsing block chain transactions linearly
/// will encounter each output before it is used as an input."
///
/// https://developer.bitcoin.org/reference/block_chain.html#merkle-trees
///
/// "each output of a particular transaction
/// can only be used as an input once in the block chain.
/// Any subsequent reference is a forbidden double spend-
/// an attempt to spend the same satoshis twice."
///
/// https://developer.bitcoin.org/devguide/block_chain.html#introduction
fn transparent_spend_chain_order(
    spend: transparent::OutPoint,
    spend_tx_index_in_block: usize,
    block_new_outputs: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    non_finalized_chain_unspent_utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
    non_finalized_chain_spent_utxos: &HashSet<transparent::OutPoint>,
    finalized_state: &FinalizedState,
) -> Result<transparent::Utxo, ValidateContextError> {
    if let Some(output) = block_new_outputs.get(&spend) {
        // reject the spend if it uses an output from this block,
        // but the output was not created by an earlier transaction
        //
        // we know the spend is invalid, because transaction IDs are unique
        //
        // (transaction IDs also commit to transaction inputs,
        // so it should be cryptographically impossible for a transaction
        // to spend its own outputs)
        if output.tx_index_in_block >= spend_tx_index_in_block {
            return Err(EarlyTransparentSpend { outpoint: spend });
        } else {
            // a unique spend of a previous transaction's output is ok
            return Ok(output.utxo.clone());
        }
    }

    if non_finalized_chain_spent_utxos.contains(&spend) {
        // reject the spend if its UTXO is already spent in the
        // non-finalized parent chain
        return Err(DuplicateTransparentSpend {
            outpoint: spend,
            location: "the non-finalized chain",
        });
    }

    match (
        non_finalized_chain_unspent_utxos.get(&spend),
        finalized_state.utxo(&spend),
    ) {
        (None, None) => {
            // we don't keep spent UTXOs in the finalized state,
            // so all we can say is that it's missing from both
            // the finalized and non-finalized chains
            // (it might have been spent in the finalized state,
            // or it might never have existed in this chain)
            Err(MissingTransparentOutput {
                outpoint: spend,
                location: "the non-finalized and finalized chain",
            })
        }

        (Some(utxo), _) => Ok(utxo.clone()),
        (_, Some(utxo)) => Ok(utxo),
    }
}

/// Check that `utxo` is spendable, based on the coinbase `spend_restriction`.
///
/// "A transaction with one or more transparent inputs from coinbase transactions
/// MUST have no transparent outputs (i.e.tx_out_count MUST be 0)."
///
/// "A transaction MUST NOT spend a transparent output of a coinbase transaction
/// from a block less than 100 blocks prior to the spend.
///
/// Note that transparent outputs of coinbase transactions include Founders’
/// Reward outputs and transparent funding stream outputs."
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn transparent_coinbase_spend(
    outpoint: transparent::OutPoint,
    spend_restriction: transparent::CoinbaseSpendRestriction,
    utxo: transparent::Utxo,
) -> Result<transparent::Utxo, ValidateContextError> {
    if !utxo.from_coinbase {
        return Ok(utxo);
    }

    match spend_restriction {
        OnlyShieldedOutputs { spend_height } => {
            let min_spend_height = utxo.height + block::Height(MIN_TRANSPARENT_COINBASE_MATURITY);
            // TODO: allow full u32 range of block heights (#1113)
            let min_spend_height =
                min_spend_height.expect("valid UTXOs have coinbase heights far below Height::MAX");
            if spend_height >= min_spend_height {
                Ok(utxo)
            } else {
                Err(ImmatureTransparentCoinbaseSpend {
                    outpoint,
                    spend_height,
                    min_spend_height,
                    created_height: utxo.height,
                })
            }
        }
        SomeTransparentOutputs => Err(UnshieldedTransparentCoinbaseSpend { outpoint }),
    }
}
