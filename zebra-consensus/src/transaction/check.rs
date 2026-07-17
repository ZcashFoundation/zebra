//! Transaction checks.
//!
//! Code in this file can freely assume that no pre-V4 transactions are present.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::Arc,
};

use chrono::{DateTime, Utc};

use zcash_script::{
    opcode::PossiblyBad,
    script::{self, Evaluable as _},
    solver, Opcode,
};
use zebra_chain::{
    amount::{Amount, NegativeAllowed, NonNegative},
    block::Height,
    orchard::Flags,
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_note_encryption,
    transaction::{LockTime, Transaction},
    transparent,
};

use crate::error::TransactionError;

/// Checks if the transaction's lock time allows this transaction to be included in a block.
///
/// Arguments:
/// - `block_height`: the height of the mined block, or the height of the next block for mempool
///   transactions
/// - `block_time`: the time in the mined block header, or the median-time-past of the next block
///   for the mempool. Optional if the lock time is a height.
///
/// # Panics
///
/// If the lock time is a time, and `block_time` is `None`.
///
/// # Consensus
///
/// > The transaction must be finalized: either its locktime must be in the past (or less
/// > than or equal to the current block height), or all of its sequence numbers must be
/// > 0xffffffff.
///
/// [`Transaction::lock_time`] validates the transparent input sequence numbers, returning [`None`]
/// if they indicate that the transaction is finalized by them.
/// Otherwise, this function checks that the lock time is in the past.
///
/// ## Mempool Consensus for Block Templates
///
/// > the nTime field MUST represent a time strictly greater than the median of the
/// > timestamps of the past PoWMedianBlockSpan blocks.
///
/// <https://zips.z.cash/protocol/protocol.pdf#blockheader>
///
/// > The transaction can be added to any block whose block time is greater than the locktime.
///
/// <https://developer.bitcoin.org/devguide/transactions.html#locktime-and-sequence-number>
///
/// If the transaction's lock time is less than the median-time-past,
/// it will always be less than the next block's time,
/// because the next block's time is strictly greater than the median-time-past.
/// (That is, `lock-time < median-time-past < block-header-time`.)
///
/// Using `median-time-past + 1s` (the next block's mintime) would also satisfy this consensus rule,
/// but we prefer the rule implemented by `zcashd`'s mempool:
/// <https://github.com/zcash/zcash/blob/9e1efad2d13dca5ee094a38e6aa25b0f2464da94/src/main.cpp#L776-L784>
pub fn lock_time_has_passed(
    tx: &Transaction,
    block_height: Height,
    block_time: impl Into<Option<DateTime<Utc>>>,
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
            let block_time = block_time
                .into()
                .expect("time must be provided if LockTime is a time");

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
/// # Consensus
///
/// > [Sapling onward] If effectiveVersion < 5, then at least one of
/// > tx_in_count, nSpendsSapling, and nJoinSplit MUST be nonzero.
///
/// > [Sapling onward] If effectiveVersion < 5, then at least one of
/// > tx_out_count, nOutputsSapling, and nJoinSplit MUST be nonzero.
///
/// > [NU5 onward] If effectiveVersion = 5 then this condition MUST hold:
/// > tx_in_count > 0 or nSpendsSapling > 0 or (nActionsOrchard > 0 and enableSpendsOrchard = 1).
///
/// > [NU5 onward] If effectiveVersion = 5 then this condition MUST hold:
/// > tx_out_count > 0 or nOutputsSapling > 0 or (nActionsOrchard > 0 and enableOutputsOrchard = 1).
///
/// > [NU6.3 onward] If effectiveVersion >= 6 then this condition MUST hold:
/// > tx_in_count > 0 or nSpendsSapling > 0 or (nActionsOrchard > 0 and enableSpendsOrchard = 1) or (nActionsIronwood > 0 and enableSpendsIronwood = 1).
///
/// > [NU6.3 onward] If effectiveVersion >= 6 then this condition MUST hold:
/// > tx_out_count > 0 or nOutputsSapling > 0 or (nActionsOrchard > 0 and enableOutputsOrchard = 1) or (nActionsIronwood > 0 and enableOutputsIronwood = 1).
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
///
/// This check counts both `Coinbase` and `PrevOut` transparent inputs.
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
/// # Consensus
///
/// For `Transaction::V5` only:
///
/// > [NU5 onward] If effectiveVersion >= 5 and nActionsOrchard > 0, then at least one of enableSpendsOrchard and enableOutputsOrchard MUST be 1.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
pub fn has_enough_orchard_flags(tx: &Transaction) -> Result<(), TransactionError> {
    if !tx.has_enough_orchard_flags() {
        return Err(TransactionError::NotEnoughOrchardFlags);
    }
    Ok(())
}

/// Checks that a transaction with Ironwood actions has at least one Ironwood flag set.
///
/// # Consensus
///
/// > [NU6.3 onward] If effectiveVersion ≥ 6 and nActionsIronwood > 0, then at least one of
/// > enableSpendsIronwood and enableOutputsIronwood MUST be 1.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
///
/// (No-op for transactions without Ironwood actions, i.e. all pre-v6 transactions.)
pub fn has_enough_ironwood_flags(tx: &Transaction) -> Result<(), TransactionError> {
    if !tx.has_enough_ironwood_flags() {
        return Err(TransactionError::NotEnoughIronwoodFlags);
    }
    Ok(())
}

/// Checks that the Orchard pool does not enable cross-address transfers (NU6.3 onward).
///
/// # Consensus
///
/// > [NU6.3 onward] The `enableCrossAddress` flag of `flagsOrchard` MUST be 0.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
///
/// An Orchard bundle can never carry this flag off the wire: bit 2 is rejected at deserialization
/// for the Orchard pool in every tx version (only the Ironwood pool permits it). So this is a
/// defense-in-depth check that also covers an in-memory-constructed bundle.
pub fn orchard_cross_address_disabled(tx: &Transaction) -> Result<(), TransactionError> {
    if let Some(orchard_shielded_data) = tx.orchard_shielded_data() {
        if orchard_shielded_data
            .flags
            .contains(Flags::ENABLE_CROSS_ADDRESS)
        {
            return Err(TransactionError::OrchardHasEnableCrossAddress);
        }
    }
    Ok(())
}

/// Checks that no net new value is shielded into the Orchard pool from NU6.3 onward.
///
/// # Consensus
///
/// > [NU6.3 onward] `valueBalanceOrchard` MUST be nonnegative.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
///
/// From NU6.3, newly shielded value is routed to the Ironwood pool, so the Orchard pool is frozen
/// against new inflows. An Orchard bundle may still spend existing notes — Orchard-to-Orchard note
/// management nets to a zero balance and Orchard-to-transparent unshielding to a positive one — but
/// a net-negative `valueBalanceOrchard` (which would move new value into the pool) is rejected.
///
/// This applies to both v5 and v6 Orchard bundles, since v5 Orchard bundles remain valid after
/// NU6.3 (so that non-upgraded hardware wallets can keep authorizing Orchard spends).
///
/// (No-op for transactions without an Orchard bundle, and before NU6.3.)
pub fn orchard_value_balance_non_negative(
    tx: &Transaction,
    network_upgrade: NetworkUpgrade,
) -> Result<(), TransactionError> {
    if network_upgrade >= NetworkUpgrade::Nu6_3 {
        if let Some(orchard_shielded_data) = tx.orchard_shielded_data() {
            if orchard_shielded_data.value_balance() < Amount::<NegativeAllowed>::zero() {
                return Err(TransactionError::NegativeOrchardValueBalance);
            }
        }
    }

    Ok(())
}

/// Checks that a coinbase transaction has an empty Orchard component from NU6.3 onward.
///
/// # Consensus
///
/// > [NU6.3 onward] Coinbase transactions MUST have an empty Orchard component.
///
/// <https://zips.z.cash/zip-0229>
///
/// From NU6.3, newly shielded coinbase value is routed to the Ironwood pool instead, so coinbase
/// transactions can no longer create Orchard notes. This is stronger than the pre-NU6.3 rule (which
/// only forbids `enableSpendsOrchard`) and applies regardless of transaction version: a v5 coinbase
/// mined at NU6.3 is constrained too, so the rule cannot be bypassed by using an older format.
///
/// (No-op for non-coinbase transactions, transactions without an Orchard component, and before
/// NU6.3.)
pub fn coinbase_orchard_component_empty(
    tx: &Transaction,
    network_upgrade: NetworkUpgrade,
) -> Result<(), TransactionError> {
    if network_upgrade >= NetworkUpgrade::Nu6_3
        && tx.is_coinbase()
        && tx.orchard_shielded_data().is_some()
    {
        return Err(TransactionError::CoinbaseHasOrchardActions);
    }

    Ok(())
}

/// Check that a coinbase transaction has no PrevOut inputs, JoinSplits, or spends.
///
/// # Consensus
///
/// > A coinbase transaction MUST NOT have any JoinSplit descriptions.
///
/// > A coinbase transaction MUST NOT have any Spend descriptions.
///
/// > [NU5 onward] In a version 5 coinbase transaction, the enableSpendsOrchard flag MUST be 0.
///
/// This check only counts `PrevOut` transparent inputs.
///
/// > [Pre-Heartwood] A coinbase transaction also MUST NOT have any Output descriptions.
///
/// Zebra does not validate this last rule explicitly because we checkpoint until Canopy activation.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
pub fn coinbase_tx_no_prevout_joinsplit_spend(tx: &Transaction) -> Result<(), TransactionError> {
    if tx.is_coinbase() {
        if tx.joinsplit_count() > 0 {
            return Err(TransactionError::CoinbaseHasJoinSplit);
        } else if tx.sapling_spends_per_anchor().count() > 0 {
            return Err(TransactionError::CoinbaseHasSpend);
        }

        if let Some(orchard_shielded_data) = tx.orchard_shielded_data() {
            if orchard_shielded_data.flags.contains(Flags::ENABLE_SPENDS) {
                return Err(TransactionError::CoinbaseHasEnableSpendsOrchard);
            }
        }

        // The stronger NU6.3 rule that a coinbase transaction must have an *empty* Orchard component
        // is height-gated and applies to every transaction version, so it lives in
        // `coinbase_orchard_component_empty` (called from `check_structure_and_network_rules`).

        // > [NU6.3 onward] In a version 6 coinbase transaction, the enableSpendsIronwood flag MUST
        // > be 0.
        //
        // (`ironwood_shielded_data` is only ever present in v6 transactions, so this is a no-op for
        // earlier versions.)
        if let Some(ironwood_shielded_data) = tx.ironwood_shielded_data() {
            if ironwood_shielded_data.flags.contains(Flags::ENABLE_SPENDS) {
                return Err(TransactionError::CoinbaseHasEnableSpendsIronwood);
            }
        }
    }

    Ok(())
}

/// Check if JoinSplits in the transaction have one of its v_{pub} values equal
/// to zero.
///
/// <https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc>
pub fn joinsplit_has_vpub_zero(tx: &Transaction) -> Result<(), TransactionError> {
    let zero = Amount::<NonNegative>::zero();

    let vpub_pairs = tx
        .output_values_to_sprout()
        .zip(tx.input_values_from_sprout());
    for (vpub_old, vpub_new) in vpub_pairs {
        // # Consensus
        //
        // > Either v_{pub}^{old} or v_{pub}^{new} MUST be zero.
        //
        // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
        if *vpub_old != zero && *vpub_new != zero {
            return Err(TransactionError::BothVPubsNonZero);
        }
    }

    Ok(())
}

/// Check if a transaction is adding to the sprout pool after Canopy
/// network upgrade given a block height and a network.
///
/// <https://zips.z.cash/zip-0211>
/// <https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc>
pub fn disabled_add_to_sprout_pool(
    tx: &Transaction,
    height: Height,
    network: &Network,
) -> Result<(), TransactionError> {
    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(network)
        .expect("Canopy activation height must be present for both networks");

    // # Consensus
    //
    // > [Canopy onward]: `vpub_old` MUST be zero.
    //
    // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
    if height >= canopy_activation_height {
        let zero = Amount::<NonNegative>::zero();

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
/// <https://developer.bitcoin.org/devguide/block_chain.html#introduction>
///
/// A _nullifier_ *MUST NOT* repeat either within a _transaction_, or across _transactions_ in a
/// _valid blockchain_ . *Sprout* and *Sapling* and *Orchard* _nulliers_ are considered disjoint,
/// even if they have the same bit pattern.
///
/// <https://zips.z.cash/protocol/protocol.pdf#nullifierset>
pub fn spend_conflicts(transaction: &Transaction) -> Result<(), TransactionError> {
    use crate::error::TransactionError::*;

    let transparent_outpoints = transaction.spent_outpoints().map(Cow::Owned);
    let sprout_nullifiers = transaction.sprout_nullifiers().map(Cow::Borrowed);
    let sapling_nullifiers = transaction.sapling_nullifiers().map(Cow::Borrowed);
    let orchard_nullifiers = transaction.orchard_nullifiers().map(Cow::Borrowed);
    // `ironwood_nullifiers()` yields owned `ironwood::Nullifier`s (the Ironwood-pool newtype), so
    // they are wrapped as `Cow::Owned`. Ironwood and Orchard nullifiers are disjoint.
    let ironwood_nullifiers = transaction.ironwood_nullifiers().map(Cow::Owned);

    check_for_duplicates(transparent_outpoints, DuplicateTransparentSpend)?;
    check_for_duplicates(sprout_nullifiers, DuplicateSproutNullifier)?;
    check_for_duplicates(sapling_nullifiers, DuplicateSaplingNullifier)?;
    check_for_duplicates(orchard_nullifiers, DuplicateOrchardNullifier)?;
    check_for_duplicates(ironwood_nullifiers, DuplicateIronwoodNullifier)?;

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
/// # Consensus
///
/// > [Heartwood onward] All Sapling and Orchard outputs in coinbase transactions MUST decrypt to a note
/// > plaintext, i.e. the procedure in § 4.20.3 ‘Decryption using a Full Viewing Key (Sapling and Orchard)’
/// > does not return ⊥, using a sequence of 32 zero bytes as the outgoing viewing key. (This implies that before
/// > Canopy activation, Sapling outputs of a coinbase transaction MUST have note plaintext lead byte equal to
/// > 0x01.)
///
/// > [Canopy onward] Any Sapling or Orchard output of a coinbase transaction decrypted to a note plaintext
/// > according to the preceding rule MUST have note plaintext lead byte equal to 0x02. (This applies even during
/// > the "grace period" specified in [ZIP-212].)
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
///
/// [ZIP-212]: https://zips.z.cash/zip-0212#consensus-rule-change-for-coinbase-transactions
///
/// TODO: Currently, a 0x01 lead byte is allowed in the "grace period" mentioned since we're
/// using `librustzcash` to implement this and it doesn't currently allow changing that behavior.
/// <https://github.com/ZcashFoundation/zebra/issues/3027>
pub fn coinbase_outputs_are_decryptable(
    transaction: &Transaction,
    network: &Network,
    height: Height,
) -> Result<(), TransactionError> {
    // Do quick checks first so we can avoid an expensive tx conversion
    // in `zcash_note_encryption::decrypts_successfully`.

    // The consensus rule only applies to coinbase txs with shielded outputs.
    if !transaction.has_shielded_outputs() {
        return Ok(());
    }

    // The consensus rule only applies to Heartwood onward.
    if height
        < NetworkUpgrade::Heartwood
            .activation_height(network)
            .expect("Heartwood height is known")
    {
        return Ok(());
    }

    // The passed tx should have been be a coinbase tx.
    if !transaction.is_coinbase() {
        return Err(TransactionError::NotCoinbase);
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
    network: &Network,
) -> Result<(), TransactionError> {
    let expiry_height = coinbase.expiry_height();

    if let Some(nu5_activation_height) = NetworkUpgrade::Nu5.activation_height(network) {
        // # Consensus
        //
        // > [NU5 onward] The nExpiryHeight field of a coinbase transaction
        // > MUST be equal to its block height.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        if *block_height >= nu5_activation_height {
            if expiry_height != Some(*block_height) {
                return Err(TransactionError::CoinbaseExpiryBlockHeight {
                    expiry_height,
                    block_height: *block_height,
                    transaction_hash: coinbase.hash(),
                });
            } else {
                return Ok(());
            }
        }
    }

    // # Consensus
    //
    // > [Overwinter to Canopy inclusive, pre-NU5] nExpiryHeight MUST be less than
    // > or equal to 499999999.
    //
    // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
    validate_expiry_height_max(expiry_height, true, block_height, coinbase)
}

/// Returns `Ok(())` if the expiry height for a non coinbase transaction is
/// valid according to specifications [7.1] and [ZIP-203].
///
/// [7.1]: https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
/// [ZIP-203]: https://zips.z.cash/zip-0203
pub fn non_coinbase_expiry_height(
    block_height: &Height,
    transaction: &Transaction,
) -> Result<(), TransactionError> {
    if transaction.is_overwintered() {
        let expiry_height = transaction.expiry_height();

        // # Consensus
        //
        // > [Overwinter to Canopy inclusive, pre-NU5] nExpiryHeight MUST be
        // > less than or equal to 499999999.
        //
        // > [NU5 onward] nExpiryHeight MUST be less than or equal to 499999999
        // > for non-coinbase transactions.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        validate_expiry_height_max(expiry_height, false, block_height, transaction)?;

        // # Consensus
        //
        // > [Overwinter onward] If a transaction is not a coinbase transaction and its
        // > nExpiryHeight field is nonzero, then it MUST NOT be mined at a block
        // > height greater than its nExpiryHeight.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        validate_expiry_height_mined(expiry_height, block_height, transaction)?;
    }
    Ok(())
}

/// Checks that the expiry height of a transaction does not exceed the maximal
/// value.
///
/// Only the `expiry_height` parameter is used for the check. The
/// remaining parameters are used to give details about the error when the check
/// fails.
fn validate_expiry_height_max(
    expiry_height: Option<Height>,
    is_coinbase: bool,
    block_height: &Height,
    transaction: &Transaction,
) -> Result<(), TransactionError> {
    if let Some(expiry_height) = expiry_height {
        if expiry_height > Height::MAX_EXPIRY_HEIGHT {
            Err(TransactionError::MaximumExpiryHeight {
                expiry_height,
                is_coinbase,
                block_height: *block_height,
                transaction_hash: transaction.hash(),
            })?;
        }
    }

    Ok(())
}

/// Checks that a transaction does not exceed its expiry height.
///
/// The `transaction` parameter is only used to give details about the error
/// when the check fails.
fn validate_expiry_height_mined(
    expiry_height: Option<Height>,
    block_height: &Height,
    transaction: &Transaction,
) -> Result<(), TransactionError> {
    if let Some(expiry_height) = expiry_height {
        if *block_height > expiry_height {
            Err(TransactionError::ExpiredTransaction {
                expiry_height,
                block_height: *block_height,
                transaction_hash: transaction.hash(),
            })?;
        }
    }

    Ok(())
}

/// Accepts a transaction, block height, block UTXOs, and
/// the transaction's spent UTXOs from the chain.
///
/// Returns `Ok(())` if spent transparent coinbase outputs are
/// valid for the block height, or a [`Err(TransactionError)`](TransactionError)
pub fn tx_transparent_coinbase_spends_maturity(
    network: &Network,
    tx: Arc<Transaction>,
    height: Height,
    block_new_outputs: Arc<HashMap<transparent::OutPoint, transparent::OrderedUtxo>>,
    spent_utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
) -> Result<(), TransactionError> {
    for spend in tx.spent_outpoints() {
        let utxo = block_new_outputs
            .get(&spend)
            .map(|ordered_utxo| ordered_utxo.utxo.clone())
            .or_else(|| spent_utxos.get(&spend).cloned())
            .expect("load_spent_utxos_fut.await should return an error if a utxo is missing");

        let spend_restriction = tx.coinbase_spend_restriction(network, height);

        zebra_state::check::transparent_coinbase_spend(spend, spend_restriction, &utxo)?;
    }

    Ok(())
}

/// The maximum number of signature operations in the redeem script of a standard P2SH input.
///
/// This is zcashd's `MAX_P2SH_SIGOPS` standardness (policy) constant:
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.h#L20>
pub const MAX_P2SH_SIGOPS: u32 = 15;

/// The maximum size in bytes of the scriptSig of a standard transaction input.
///
/// This is zcashd's `MAX_STANDARD_SCRIPTSIG_SIZE` standardness (policy) constant:
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.cpp#L92-L99>
pub const MAX_STANDARD_SCRIPTSIG_SIZE: usize = 1650;

/// Classify a script using the `zcash_script` solver.
///
/// Returns `Some(kind)` for standard script types, `None` for non-standard.
///
/// Mirrors the classification done by zcashd's `Solver()`.
pub fn standard_script_kind(lock_script: &transparent::Script) -> Option<solver::ScriptKind> {
    let code = script::Code(lock_script.as_raw_bytes().to_vec());
    let component = code.to_component().ok()?.refine().ok()?;
    solver::standard(&component)
}

/// Returns the expected number of scriptSig arguments for a given script kind.
///
/// Mirrors zcashd's `ScriptSigArgsExpected()`:
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/script/standard.cpp#L135>
///
/// Returns `None` for non-standard types (TX_NONSTANDARD, TX_NULL_DATA).
pub(super) fn script_sig_args_expected(kind: &solver::ScriptKind) -> Option<usize> {
    match kind {
        solver::ScriptKind::PubKey { .. } => Some(1),
        solver::ScriptKind::PubKeyHash { .. } => Some(2),
        solver::ScriptKind::ScriptHash { .. } => Some(1),
        solver::ScriptKind::MultiSig { required, .. } => Some(*required as usize + 1),
        solver::ScriptKind::NullData { .. } => None,
    }
}

/// Extract the redeemed script bytes from a P2SH scriptSig.
///
/// The redeemed script is the last data push in the scriptSig.
/// Returns `None` if the scriptSig has no push operations.
///
/// # Precondition
///
/// The scriptSig should be push-only (enforced by [`mempool_standard_input_scripts`] before this
/// function is reached). Non-push opcodes are silently ignored.
pub(super) fn extract_p2sh_redeemed_script(unlock_script: &transparent::Script) -> Option<Vec<u8>> {
    let code = script::Code(unlock_script.as_raw_bytes().to_vec());
    let mut last_push_data: Option<Vec<u8>> = None;
    for opcode in code.parse().flatten() {
        if let PossiblyBad::Good(Opcode::PushValue(pv)) = opcode {
            last_push_data = Some(pv.value());
        }
    }
    last_push_data
}

/// Count the number of push operations in a script.
///
/// For a push-only script (already enforced for mempool scriptSigs),
/// this equals the stack depth after evaluation.
pub(super) fn count_script_push_ops(script_bytes: &[u8]) -> usize {
    let code = script::Code(script_bytes.to_vec());
    code.parse()
        .filter(|op| matches!(op, Ok(PossiblyBad::Good(Opcode::PushValue(_)))))
        .count()
}

/// Returns `true` if all of a transaction's transparent inputs are standard.
///
/// Mirrors zcashd's `AreInputsStandard()`:
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.cpp#L136>
///
/// For each input:
/// 1. The spent output's scriptPubKey must be a known standard type (via the `zcash_script`
///    solver). Non-standard scripts and OP_RETURN outputs are rejected.
/// 2. The scriptSig stack depth must match `ScriptSigArgsExpected()`.
/// 3. For P2SH inputs:
///    - If the redeemed script is standard, its expected args are added to the total.
///    - If the redeemed script is non-standard, it must have at most [`MAX_P2SH_SIGOPS`] sigops.
///
/// # Correctness
///
/// Callers must ensure `spent_outputs.len()` matches the number of transparent inputs.
/// If the lengths differ, `false` is returned.
pub fn are_inputs_standard(tx: &Transaction, spent_outputs: &[transparent::Output]) -> bool {
    if tx.inputs().len() != spent_outputs.len() {
        return false;
    }
    for (input, spent_output) in tx.inputs().iter().zip(spent_outputs.iter()) {
        let unlock_script = match input {
            transparent::Input::PrevOut { unlock_script, .. } => unlock_script,
            transparent::Input::Coinbase { .. } => continue,
        };

        // Step 1: Classify the spent output's scriptPubKey via the zcash_script solver.
        let script_kind = match standard_script_kind(&spent_output.lock_script) {
            Some(kind) => kind,
            None => return false,
        };

        // Step 2: Get expected number of scriptSig arguments.
        // Returns None for TX_NONSTANDARD and TX_NULL_DATA.
        let mut n_args_expected = match script_sig_args_expected(&script_kind) {
            Some(n) => n,
            None => return false,
        };

        // Step 3: Count actual push operations in scriptSig.
        // For push-only scripts (enforced before this function), this equals the stack depth.
        let stack_size = count_script_push_ops(unlock_script.as_raw_bytes());

        // Step 4: P2SH-specific checks.
        if matches!(script_kind, solver::ScriptKind::ScriptHash { .. }) {
            let Some(redeemed_bytes) = extract_p2sh_redeemed_script(unlock_script) else {
                return false;
            };

            let redeemed_code = script::Code(redeemed_bytes);

            // Classify the redeemed script using the zcash_script solver.
            let redeemed_kind = {
                let component = redeemed_code
                    .to_component()
                    .ok()
                    .and_then(|c| c.refine().ok());
                component.and_then(|c| solver::standard(&c))
            };

            match redeemed_kind {
                Some(ref inner_kind) => {
                    // Standard redeemed script: add its expected args.
                    match script_sig_args_expected(inner_kind) {
                        Some(inner) => n_args_expected += inner,
                        None => return false,
                    }
                }
                None => {
                    // Non-standard redeemed script: accept if sigops <= limit.
                    // Matches zcashd: "Any other Script with less than 15 sigops OK:
                    // ... extra data left on the stack after execution is OK, too"
                    let sigops = redeemed_code.sig_op_count(true);
                    if sigops > MAX_P2SH_SIGOPS {
                        return false;
                    }

                    // This input is acceptable; move on to the next input.
                    continue;
                }
            }
        }

        // Step 5: Reject if scriptSig has wrong number of stack items.
        if stack_size != n_args_expected {
            return false;
        }
    }
    true
}

/// Standardness (policy) checks on a mempool transaction's transparent input scripts, applied
/// *before* the transaction is dispatched to script verification. The goal is to avoid the
/// expensive verification for non-standard transactions which would be rejected anyway
/// by `Storage::reject_if_non_standard_tx()`; this is a subset of the checks
/// in that function.
///
/// `spent_outputs` must contain the output spent by each of the transaction's transparent inputs,
/// in input order.
///
/// # Correctness
///
/// `spent_outputs.len()` must equal the number of transparent inputs in `tx`: if the lengths
/// differ, `zip()` silently truncates, and some inputs are not checked.
pub fn mempool_standard_input_scripts(
    tx: &Transaction,
    spent_outputs: &[transparent::Output],
) -> Result<(), TransactionError> {
    if tx.inputs().len() != spent_outputs.len() {
        return Err(TransactionError::Other(format!(
            "spent_outputs must align with transaction inputs for non-coinbase txs: inputs={}, spent_outputs={}",
            tx.inputs().len(),
            spent_outputs.len(),
        )));
    }

    for (input_index, input) in tx.inputs().iter().enumerate() {
        let unlock_script = match input {
            transparent::Input::PrevOut { unlock_script, .. } => unlock_script,
            transparent::Input::Coinbase { .. } => continue,
        };

        // Rule: the scriptSig must be within the standard size limit.
        let size = unlock_script.as_raw_bytes().len();
        if size > MAX_STANDARD_SCRIPTSIG_SIZE {
            return Err(TransactionError::NonStandardScriptSigSize { input_index, size });
        }

        // Rule: the scriptSig must be push-only.
        if !script::Code(unlock_script.as_raw_bytes().to_vec()).is_push_only() {
            return Err(TransactionError::NonStandardScriptSigNotPushOnly { input_index });
        }
    }

    // Rule: all transparent inputs must pass `AreInputsStandard()` checks:
    // https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.cpp#L137
    if !are_inputs_standard(tx, spent_outputs) {
        return Err(TransactionError::NonStandardInputs);
    }

    Ok(())
}

/// Checks the `nConsensusBranchId` field.
///
/// # Consensus
///
/// ## [7.1.2 Transaction Consensus Rules]
///
/// > [**NU5** onward] If `effectiveVersion` ≥ 5, the `nConsensusBranchId` field **MUST** match the
/// > consensus branch ID used for SIGHASH transaction hashes, as specified in [ZIP-244].
///
/// ### Notes
///
/// - When deserializing transactions, Zebra converts the `nConsensusBranchId` into
///   [`NetworkUpgrade`].
///
/// - The values returned by [`Transaction::version`] match `effectiveVersion` so we use them in
///   place of `effectiveVersion`. More details in [`Transaction::version`].
///
/// [ZIP-244]: <https://zips.z.cash/zip-0244>
/// [7.1.2 Transaction Consensus Rules]: <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
pub fn consensus_branch_id(
    tx: &Transaction,
    height: Height,
    network: &Network,
) -> Result<(), TransactionError> {
    let current_nu = NetworkUpgrade::current(network, height);

    if current_nu < NetworkUpgrade::Nu5 || tx.version() < 5 {
        return Ok(());
    }

    let Some(tx_nu) = tx.network_upgrade() else {
        return Err(TransactionError::MissingConsensusBranchId);
    };

    if tx_nu != current_nu {
        return Err(TransactionError::WrongConsensusBranchId);
    }

    Ok(())
}
