//! Consensus rule checks for the finalized state.

use std::collections::HashMap;

use zebra_chain::{
    amount,
    transparent::{self, CoinbaseSpendRestriction::*},
};

use crate::{
    constants::MIN_TRANSPARENT_COINBASE_MATURITY,
    service::{finalized_state::ZebraDb, non_finalized_state::SpendingTransactionId},
    SemanticallyVerifiedBlock,
    ValidateContextError::{
        self, DuplicateTransparentSpend, EarlyTransparentSpend, ImmatureTransparentCoinbaseSpend,
        MissingTransparentOutput, UnshieldedTransparentCoinbaseSpend,
    },
};

/// Lookup all the [`transparent::Utxo`]s spent by a [`SemanticallyVerifiedBlock`].
/// If any of the spends are invalid, return an error.
/// Otherwise, return the looked up UTXOs.
///
/// Checks for the following kinds of invalid spends:
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
    semantically_verified: &SemanticallyVerifiedBlock,
    non_finalized_chain_unspent_utxos: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    non_finalized_chain_spent_utxos: &HashMap<transparent::OutPoint, SpendingTransactionId>,
    finalized_state: &ZebraDb,
) -> Result<HashMap<transparent::OutPoint, transparent::OrderedUtxo>, ValidateContextError> {
    let mut block_spends = HashMap::new();

    for (spend_tx_index_in_block, transaction) in
        semantically_verified.block.transactions.iter().enumerate()
    {
        // Coinbase inputs represent new coins,
        // so there are no UTXOs to mark as spent.
        let spends = transaction
            .inputs()
            .iter()
            .filter_map(transparent::Input::outpoint);

        for spend in spends {
            let utxo = transparent_spend_chain_order(
                spend,
                spend_tx_index_in_block,
                &semantically_verified.new_outputs,
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
            let spend_restriction = transaction.coinbase_spend_restriction(
                &finalized_state.network(),
                semantically_verified.height,
            );
            transparent_coinbase_spend(spend, spend_restriction, utxo.as_ref())?;

            // We don't delete the UTXOs until the block is committed,
            // so we  need to check for duplicate spends within the same block.
            //
            // See `transparent_spend_chain_order` for the relevant consensus rule.
            if block_spends.insert(spend, utxo).is_some() {
                return Err(DuplicateTransparentSpend {
                    outpoint: spend,
                    location: "the same block",
                });
            }
        }
    }

    remaining_transaction_value(semantically_verified, &block_spends)?;

    Ok(block_spends)
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
/// <https://developer.bitcoin.org/reference/block_chain.html#merkle-trees>
///
/// "each output of a particular transaction
/// can only be used as an input once in the block chain.
/// Any subsequent reference is a forbidden double spend-
/// an attempt to spend the same satoshis twice."
///
/// <https://developer.bitcoin.org/devguide/block_chain.html#introduction>
///
/// # Consensus
///
/// > Every non-null prevout MUST point to a unique UTXO in either a preceding block,
/// > or a previous transaction in the same block.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
fn transparent_spend_chain_order(
    spend: transparent::OutPoint,
    spend_tx_index_in_block: usize,
    block_new_outputs: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    non_finalized_chain_unspent_utxos: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    non_finalized_chain_spent_utxos: &HashMap<transparent::OutPoint, SpendingTransactionId>,
    finalized_state: &ZebraDb,
) -> Result<transparent::OrderedUtxo, ValidateContextError> {
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
            return Ok(output.clone());
        }
    }

    if non_finalized_chain_spent_utxos.contains_key(&spend) {
        // reject the spend if its UTXO is already spent in the
        // non-finalized parent chain
        return Err(DuplicateTransparentSpend {
            outpoint: spend,
            location: "the non-finalized chain",
        });
    }

    non_finalized_chain_unspent_utxos
        .get(&spend)
        .cloned()
        .or_else(|| finalized_state.utxo(&spend))
        // we don't keep spent UTXOs in the finalized state,
        // so all we can say is that it's missing from both
        // the finalized and non-finalized chains
        // (it might have been spent in the finalized state,
        // or it might never have existed in this chain)
        .ok_or(MissingTransparentOutput {
            outpoint: spend,
            location: "the non-finalized and finalized chain",
        })
}

/// Check that `utxo` is spendable, based on the coinbase `spend_restriction`.
///
/// # Consensus
///
/// > A transaction with one or more transparent inputs from coinbase transactions
/// > MUST have no transparent outputs (i.e. tx_out_count MUST be 0).
/// > Inputs from coinbase transactions include Founders’ Reward outputs and
/// > funding stream outputs.
///
/// > A transaction MUST NOT spend a transparent output of a coinbase transaction
/// > from a block less than 100 blocks prior to the spend.
/// > Note that transparent outputs of coinbase transactions include
/// > Founders’ Reward outputs and transparent funding stream outputs.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
pub fn transparent_coinbase_spend(
    outpoint: transparent::OutPoint,
    spend_restriction: transparent::CoinbaseSpendRestriction,
    utxo: &transparent::Utxo,
) -> Result<(), ValidateContextError> {
    if !utxo.from_coinbase {
        return Ok(());
    }

    match spend_restriction {
        CheckCoinbaseMaturity { spend_height } => {
            let min_spend_height = utxo.height + MIN_TRANSPARENT_COINBASE_MATURITY.into();
            let min_spend_height =
                min_spend_height.expect("valid UTXOs have coinbase heights far below Height::MAX");
            if spend_height >= min_spend_height {
                Ok(())
            } else {
                Err(ImmatureTransparentCoinbaseSpend {
                    outpoint,
                    spend_height,
                    min_spend_height,
                    created_height: utxo.height,
                })
            }
        }
        DisallowCoinbaseSpend => Err(UnshieldedTransparentCoinbaseSpend { outpoint }),
    }
}

/// Reject negative remaining transaction value.
///
/// "As in Bitcoin, the remaining value in the transparent transaction value pool
/// of a non-coinbase transaction is available to miners as a fee.
///
/// The remaining value in the transparent transaction value pool of a
/// coinbase transaction is destroyed.
///
/// Consensus rule: The remaining value in the transparent transaction value pool
/// MUST be nonnegative."
///
/// <https://zips.z.cash/protocol/protocol.pdf#transactions>
pub fn remaining_transaction_value(
    semantically_verified: &SemanticallyVerifiedBlock,
    utxos: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
) -> Result<(), ValidateContextError> {
    for (tx_index_in_block, transaction) in
        semantically_verified.block.transactions.iter().enumerate()
    {
        if transaction.is_coinbase() {
            continue;
        }

        // Build a temporary UTXO map (OutPoint -> Utxo) from the provided
        // OrderedUtxo references, avoiding cloning the entire map.
        let utxos_map: HashMap<_, _> = utxos.iter().map(|(k, v)| (*k, v.utxo.clone())).collect();

        // Check the remaining transparent value pool for this transaction
        let value_balance = transaction.value_balance(&utxos_map);
        match value_balance {
            Ok(vb) => match vb.remaining_transaction_value() {
                Ok(_) => Ok(()),
                Err(amount_error @ amount::Error::Constraint { .. })
                    if amount_error.invalid_value() < 0 =>
                {
                    Err(ValidateContextError::NegativeRemainingTransactionValue {
                        amount_error,
                        height: semantically_verified.height,
                        tx_index_in_block,
                        transaction_hash: semantically_verified.transaction_hashes
                            [tx_index_in_block],
                    })
                }
                Err(amount_error) => {
                    Err(ValidateContextError::CalculateRemainingTransactionValue {
                        amount_error,
                        height: semantically_verified.height,
                        tx_index_in_block,
                        transaction_hash: semantically_verified.transaction_hashes
                            [tx_index_in_block],
                    })
                }
            },
            Err(value_balance_error) => {
                Err(ValidateContextError::CalculateTransactionValueBalances {
                    value_balance_error,
                    height: semantically_verified.height,
                    tx_index_in_block,
                    transaction_hash: semantically_verified.transaction_hashes[tx_index_in_block],
                })
            }
        }?
    }

    Ok(())
}
