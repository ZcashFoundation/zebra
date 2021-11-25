//! Transaction checks.
//!
//! Code in this file can freely assume that no pre-V4 transactions are present.

use std::{borrow::Cow, collections::HashSet, convert::TryFrom, hash::Hash};

use chrono::{DateTime, Utc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::Height,
    orchard::Flags,
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_note_encryption,
    sapling::{Output, PerSpendAnchor, Spend},
    transaction::{LockTime, Transaction},
};

use crate::error::TransactionError;

/// Checks if the transaction's lock time allows this transaction to be included in a block.
///
/// Consensus rule:
///
/// > The transaction must be finalized: either its locktime must be in the past (or less
/// > than or equal to the current block height), or all of its sequence numbers must be
/// > 0xffffffff.
///
/// [`Transaction::lock_time`] validates the transparent input sequence numbers, returning [`None`]
/// if they indicate that the transaction is finalized by them. Otherwise, this function validates
/// if the lock time is in the past.
pub fn lock_time_has_passed(
    tx: &Transaction,
    block_height: Height,
    block_time: DateTime<Utc>,
) -> Result<(), TransactionError> {
    match tx.lock_time() {
        Some(LockTime::Height(unlock_height)) => {
            // > The transaction can be added to any block which has a greater height.
            // The Bitcoin documentation is wrong or outdated here,
            // so this code is based on the `zcashd` implementation at:
            // https://github.com/zcash/zcash/blob/1a7c2a3b04bcad6549be6d571bfdff8af9a2c814/src/main.cpp#L722
            if block_height > unlock_height {
                Ok(())
            } else {
                Err(TransactionError::LockedUntilAfterBlockHeight(unlock_height))
            }
        }
        Some(LockTime::Time(unlock_time)) => {
            // > The transaction can be added to any block whose block time is greater than the locktime.
            // https://developer.bitcoin.org/devguide/transactions.html#locktime-and-sequence-number
            if block_time > unlock_time {
                Ok(())
            } else {
                Err(TransactionError::LockedUntilAfterBlockTime(unlock_time))
            }
        }
        None => Ok(()),
    }
}

/// Checks that the transaction has inputs and outputs.
///
/// For `Transaction::V4`:
/// * At least one of `tx_in_count`, `nSpendsSapling`, and `nJoinSplit` MUST be non-zero.
/// * At least one of `tx_out_count`, `nOutputsSapling`, and `nJoinSplit` MUST be non-zero.
///
/// For `Transaction::V5`:
/// * This condition must hold: `tx_in_count` > 0 or `nSpendsSapling` > 0 or
/// (`nActionsOrchard` > 0 and `enableSpendsOrchard` = 1)
/// * This condition must hold: `tx_out_count` > 0 or `nOutputsSapling` > 0 or
/// (`nActionsOrchard` > 0 and `enableOutputsOrchard` = 1)
///
/// This check counts both `Coinbase` and `PrevOut` transparent inputs.
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn has_inputs_and_outputs(tx: &Transaction) -> Result<(), TransactionError> {
    if !tx.has_transparent_or_shielded_inputs() {
        Err(TransactionError::NoInputs)
    } else if !tx.has_transparent_or_shielded_outputs() {
        Err(TransactionError::NoOutputs)
    } else {
        Ok(())
    }
}

/// Checks that the transaction has enough orchard flags.
///
/// For `Transaction::V5` only:
/// * If `orchard_actions_count` > 0 then at least one of
/// `ENABLE_SPENDS|ENABLE_OUTPUTS` must be active.
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn has_enough_orchard_flags(tx: &Transaction) -> Result<(), TransactionError> {
    if !tx.has_enough_orchard_flags() {
        return Err(TransactionError::NotEnoughFlags);
    }
    Ok(())
}

/// Check that a coinbase transaction has no PrevOut inputs, JoinSplits, or spends.
///
/// A coinbase transaction MUST NOT have any transparent inputs, JoinSplit descriptions,
/// or Spend descriptions.
///
/// In a version 5 coinbase transaction, the enableSpendsOrchard flag MUST be 0.
///
/// This check only counts `PrevOut` transparent inputs.
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn coinbase_tx_no_prevout_joinsplit_spend(tx: &Transaction) -> Result<(), TransactionError> {
    if tx.has_valid_coinbase_transaction_inputs() {
        if tx.contains_prevout_input() {
            return Err(TransactionError::CoinbaseHasPrevOutInput);
        } else if tx.joinsplit_count() > 0 {
            return Err(TransactionError::CoinbaseHasJoinSplit);
        } else if tx.sapling_spends_per_anchor().count() > 0 {
            return Err(TransactionError::CoinbaseHasSpend);
        }

        if let Some(orchard_shielded_data) = tx.orchard_shielded_data() {
            if orchard_shielded_data.flags.contains(Flags::ENABLE_SPENDS) {
                return Err(TransactionError::CoinbaseHasEnableSpendsOrchard);
            }
        }
    }

    Ok(())
}

/// Check that a Spend description's cv and rk are not of small order,
/// i.e. [h_J]cv MUST NOT be 𝒪_J and [h_J]rk MUST NOT be 𝒪_J.
///
/// https://zips.z.cash/protocol/protocol.pdf#spenddesc
pub fn spend_cv_rk_not_small_order(spend: &Spend<PerSpendAnchor>) -> Result<(), TransactionError> {
    if bool::from(spend.cv.0.is_small_order())
        || bool::from(
            jubjub::AffinePoint::from_bytes(spend.rk.into())
                .unwrap()
                .is_small_order(),
        )
    {
        Err(TransactionError::SmallOrder)
    } else {
        Ok(())
    }
}

/// Check that a Output description's cv and epk are not of small order,
/// i.e. [h_J]cv MUST NOT be 𝒪_J and [h_J]epk MUST NOT be 𝒪_J.
///
/// https://zips.z.cash/protocol/protocol.pdf#outputdesc
pub fn output_cv_epk_not_small_order(output: &Output) -> Result<(), TransactionError> {
    if bool::from(output.cv.0.is_small_order())
        || bool::from(
            jubjub::AffinePoint::from_bytes(output.ephemeral_key.into())
                .unwrap()
                .is_small_order(),
        )
    {
        Err(TransactionError::SmallOrder)
    } else {
        Ok(())
    }
}

/// Check if a transaction is adding to the sprout pool after Canopy
/// network upgrade given a block height and a network.
///
/// https://zips.z.cash/zip-0211
/// https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
pub fn disabled_add_to_sprout_pool(
    tx: &Transaction,
    height: Height,
    network: Network,
) -> Result<(), TransactionError> {
    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(network)
        .expect("Canopy activation height must be present for both networks");

    // [Canopy onward]: `vpub_old` MUST be zero.
    // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
    if height >= canopy_activation_height {
        let zero = Amount::<NonNegative>::try_from(0).expect("an amount of 0 is always valid");

        let tx_sprout_pool = tx.output_values_to_sprout();
        for vpub_old in tx_sprout_pool {
            if *vpub_old != zero {
                return Err(TransactionError::DisabledAddToSproutPool);
            }
        }
    }

    Ok(())
}

/// Check if a transaction has any internal spend conflicts.
///
/// An internal spend conflict happens if the transaction spends a UTXO more than once or if it
/// reveals a nullifier more than once.
///
/// Consensus rules:
///
/// "each output of a particular transaction
/// can only be used as an input once in the block chain.
/// Any subsequent reference is a forbidden double spend-
/// an attempt to spend the same satoshis twice."
///
/// https://developer.bitcoin.org/devguide/block_chain.html#introduction
///
/// A _nullifier_ *MUST NOT* repeat either within a _transaction_, or across _transactions_ in a
/// _valid blockchain_ . *Sprout* and *Sapling* and *Orchard* _nulliers_ are considered disjoint,
/// even if they have the same bit pattern.
///
/// https://zips.z.cash/protocol/protocol.pdf#nullifierset
pub fn spend_conflicts(transaction: &Transaction) -> Result<(), TransactionError> {
    use crate::error::TransactionError::*;

    let transparent_outpoints = transaction.spent_outpoints().map(Cow::Owned);
    let sprout_nullifiers = transaction.sprout_nullifiers().map(Cow::Borrowed);
    let sapling_nullifiers = transaction.sapling_nullifiers().map(Cow::Borrowed);
    let orchard_nullifiers = transaction.orchard_nullifiers().map(Cow::Borrowed);

    check_for_duplicates(transparent_outpoints, DuplicateTransparentSpend)?;
    check_for_duplicates(sprout_nullifiers, DuplicateSproutNullifier)?;
    check_for_duplicates(sapling_nullifiers, DuplicateSaplingNullifier)?;
    check_for_duplicates(orchard_nullifiers, DuplicateOrchardNullifier)?;

    Ok(())
}

/// Check for duplicate items in a collection.
///
/// Each item should be wrapped by a [`Cow`] instance so that this helper function can properly
/// handle borrowed items and owned items.
///
/// If a duplicate is found, an error created by the `error_wrapper` is returned.
fn check_for_duplicates<'t, T>(
    items: impl IntoIterator<Item = Cow<'t, T>>,
    error_wrapper: impl FnOnce(T) -> TransactionError,
) -> Result<(), TransactionError>
where
    T: Clone + Eq + Hash + 't,
{
    let mut hash_set = HashSet::new();

    for item in items {
        if let Some(duplicate) = hash_set.replace(item) {
            return Err(error_wrapper(duplicate.into_owned()));
        }
    }

    Ok(())
}

/// Checks compatibility with [ZIP-212] shielded Sapling and Orchard coinbase output decryption
///
/// Pre-Heartwood: returns `Ok`.
/// Heartwood-onward: returns `Ok` if all Sapling or Orchard outputs, if any, decrypt successfully with
/// an all-zeroes outgoing viewing key. Returns `Err` otherwise.
///
/// This is used to validate coinbase transactions:
///
/// > [Heartwood onward] All Sapling and Orchard outputs in coinbase transactions MUST decrypt to a note
/// > plaintext, i.e. the procedure in § 4.19.3 ‘Decryption using a Full Viewing Key ( Sapling and Orchard )’ on p. 67
/// > does not return ⊥, using a sequence of 32 zero bytes as the outgoing viewing key. (This implies that before
/// > Canopy activation, Sapling outputs of a coinbase transaction MUST have note plaintext lead byte equal to
/// > 0x01.)
///
/// > [Canopy onward] Any Sapling or Orchard output of a coinbase transaction decrypted to a note plaintext
/// > according to the preceding rule MUST have note plaintext lead byte equal to 0x02. (This applies even during
/// > the "grace period" specified in [ZIP-212].)
///
/// [3.10]: https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions
/// [ZIP-212]: https://zips.z.cash/zip-0212#consensus-rule-change-for-coinbase-transactions
///
/// TODO: Currently, a 0x01 lead byte is allowed in the "grace period" mentioned since we're
/// using `librustzcash` to implement this and it doesn't currently allow changing that behavior.
/// https://github.com/ZcashFoundation/zebra/issues/3027
pub fn coinbase_outputs_are_decryptable(
    transaction: &Transaction,
    network: Network,
    height: Height,
) -> Result<(), TransactionError> {
    // The consensus rule only applies to Heartwood onward.
    if height
        < NetworkUpgrade::Heartwood
            .activation_height(network)
            .expect("Heartwood height is known")
    {
        return Ok(());
    }

    if !zcash_note_encryption::decrypts_successfully(transaction, network, height) {
        return Err(TransactionError::CoinbaseOutputsNotDecryptable);
    }

    Ok(())
}

/// Returns `Ok(())` if the expiry height for the coinbase transaction is valid
/// according to specifications [7.1] and [ZIP-203].
///
/// [7.1]: https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
/// [ZIP-203]: https://zips.z.cash/zip-0203
pub fn coinbase_expiry_height(
    block_height: &Height,
    coinbase: &Transaction,
    network: Network,
) -> Result<(), TransactionError> {
    match NetworkUpgrade::Nu5.activation_height(network) {
        // If Nu5 does not have a height, apply the pre-Nu5 rule.
        None => validate_expiry_height_max(coinbase.expiry_height()),
        Some(activation_height) => {
            // Conesnsus rule: from NU5 activation, the nExpiryHeight field of a
            // coinbase transaction MUST be set equal to the block height.
            if *block_height >= activation_height {
                match coinbase.expiry_height() {
                    None => Err(TransactionError::TransactionExpiration)?,
                    Some(expiry) => {
                        if expiry != *block_height {
                            return Err(TransactionError::TransactionExpiration)?;
                        }
                    }
                }
                return Ok(());
            }
            // Consensus rule: [Overwinter to Canopy inclusive, pre-NU5] nExpiryHeight
            // MUST be less than or equal to 499999999.
            validate_expiry_height_max(coinbase.expiry_height())
        }
    }
}

/// Returns `Ok(())` if the expiry height for a non coinbase transaction is valid
/// according to specifications [7.1] and [ZIP-203].
///
/// [7.1]: https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
/// [ZIP-203]: https://zips.z.cash/zip-0203
pub fn non_coinbase_expiry_height(
    block_height: &Height,
    transaction: &Transaction,
) -> Result<(), TransactionError> {
    if transaction.is_overwintered() {
        let expiry_height = transaction.expiry_height();

        validate_expiry_height_max(expiry_height)?;
        validate_expiry_height_mined(expiry_height, block_height)?;
    }
    Ok(())
}

/// Validate the consensus rule: nExpiryHeight MUST be less than or equal to 499999999.
fn validate_expiry_height_max(expiry_height: Option<Height>) -> Result<(), TransactionError> {
    if let Some(expiry) = expiry_height {
        if expiry > Height::MAX_EXPIRY_HEIGHT {
            return Err(TransactionError::TransactionExpiration)?;
        }
    }

    Ok(())
}

/// Validate the consensus rule: If a transaction is not a coinbase transaction
/// and its nExpiryHeight field is nonzero, then it MUST NOT be mined at a block
/// height greater than its nExpiryHeight.
fn validate_expiry_height_mined(
    expiry_height: Option<Height>,
    block_height: &Height,
) -> Result<(), TransactionError> {
    if let Some(expiry) = expiry_height {
        if *block_height > expiry {
            return Err(TransactionError::TransactionExpiration)?;
        }
    }

    Ok(())
}
