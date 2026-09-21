//! Block subsidy, funding stream, and miner fee consensus checks.

use mset::MultiSet;

use crate::{
    amount::{
        Amount, DeferredPoolBalanceChange, Error as AmountError, NegativeAllowed, NonNegative,
    },
    block::{Block, Height},
    parameters::{Network, NetworkUpgrade},
    transaction::Transaction,
    transparent::{Address, Output},
};

use super::{
    founders_reward, founders_reward_address, funding_stream_address, funding_stream_values,
    nsm_fee_contribution, FundingStreamReceiver, ParameterSubsidy, SubsidyError,
};

/// An error from the block subsidy or miner fee checks on a block's coinbase transaction.
#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
#[non_exhaustive]
pub enum CoinbaseTransactionError {
    #[error(transparent)]
    Subsidy(#[from] SubsidyError),

    #[error("founders reward address must be defined for height: {0:?}")]
    FoundersRewardAddressNotFound(Height),

    #[error("missing lockbox disbursements for NU6.1 activation block")]
    MissingLockboxDisbursements,

    #[error("A funding stream other than the deferred pool must have an address")]
    FundingStreamAddressNotFound,

    #[error("invalid transparent value balance: {0}")]
    InvalidTransparentValueBalance(AmountError),
}

impl From<AmountError> for CoinbaseTransactionError {
    fn from(error: AmountError) -> Self {
        Self::Subsidy(error.into())
    }
}

/// Returns `Ok()` with the deferred pool balance change of the coinbase transaction if the block
/// subsidy in `block` is valid for `network`
///
/// [3.9]: https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts
pub fn subsidy_is_valid(
    block: &Block,
    net: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<DeferredPoolBalanceChange, CoinbaseTransactionError> {
    if expected_block_subsidy.is_zero() {
        return Ok(DeferredPoolBalanceChange::zero());
    }

    let height = block.coinbase_height().ok_or(SubsidyError::NoCoinbase)?;

    let mut coinbase_outputs: MultiSet<Output> = block
        .transactions
        .first()
        .ok_or(SubsidyError::NoCoinbase)?
        .outputs()
        .iter()
        .cloned()
        .collect();

    let mut has_amount = |addr: &Address, amount| {
        assert!(addr.is_script_hash(), "address must be P2SH");

        coinbase_outputs.remove(&Output::new(amount, addr.script()))
    };

    // # Note
    //
    // Canopy activation is at the first halving on Mainnet, but not on Testnet. [ZIP-1014] only
    // applies to Mainnet; [ZIP-214] contains the specific rules for Testnet funding stream amount
    // values.
    //
    // [ZIP-1014]: <https://zips.z.cash/zip-1014>
    // [ZIP-214]: <https://zips.z.cash/zip-0214
    if NetworkUpgrade::current(net, height) < NetworkUpgrade::Canopy {
        // # Consensus
        //
        // > [Pre-Canopy] A coinbase transaction at `height ∈ {1 .. FoundersRewardLastBlockHeight}`
        // > MUST include at least one output that pays exactly `FoundersReward(height)` zatoshi
        // > with a standard P2SH script of the form `OP_HASH160 FounderRedeemScriptHash(height)
        // > OP_EQUAL` as its `scriptPubKey`.
        //
        // ## Notes
        //
        // - `FoundersRewardLastBlockHeight := max({height : N | Halving(height) < 1})`
        //
        // <https://zips.z.cash/protocol/protocol.pdf#foundersreward>

        if Height::MIN < height && height < net.height_for_first_halving() {
            let addr = founders_reward_address(net, height).ok_or(
                CoinbaseTransactionError::FoundersRewardAddressNotFound(height),
            )?;

            if !has_amount(&addr, founders_reward(net, height)) {
                Err(SubsidyError::FoundersRewardNotFound)?;
            }
        }

        Ok(DeferredPoolBalanceChange::zero())
    } else {
        // # Consensus
        //
        // > [Canopy onward] In each block with coinbase transaction `cb` at block height `height`,
        // > `cb` MUST contain at least the given number of distinct outputs for each of the
        // > following:
        //
        // > • for each funding stream `fs` active at that block height with a recipient identifier
        // > other than `DEFERRED_POOL` given by `fs.Recipient(height)`, one output that pays
        // > `fs.Value(height)` zatoshi in the prescribed way to the address represented by that
        // > recipient identifier;
        //
        // > • [NU6.1 onward] if the block height is `ZIP271ActivationHeight`,
        // > `ZIP271DisbursementChunks` equal outputs paying a total of `ZIP271DisbursementAmount`
        // > zatoshi in the prescribed way to the Key-Holder Organizations’ P2SH multisig address
        // > represented by `ZIP271DisbursementAddress`, as specified by [ZIP-271].
        //
        // > The term “prescribed way” is defined as follows:
        //
        // > The prescribed way to pay a transparent P2SH address is to use a standard P2SH script
        // > of the form `OP_HASH160 fs.RedeemScriptHash(height) OP_EQUAL` as the `scriptPubKey`.
        // > Here `fs.RedeemScriptHash(height)` is the standard redeem script hash for the recipient
        // > address for `fs.Recipient(height)` in _Base58Check_ form. Standard redeem script hashes
        // > are defined in [ZIP-48] for P2SH multisig addresses, or [Bitcoin-P2SH] for other P2SH
        // > addresses.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#fundingstreams>
        //
        // [ZIP-271]: <https://zips.z.cash/zip-0271>
        // [ZIP-48]: <https://zips.z.cash/zip-0048>
        // [Bitcoin-P2SH]: <https://developer.bitcoin.org/devguide/transactions.html#pay-to-script-hash-p2sh>

        let mut funding_streams = funding_stream_values(height, net, expected_block_subsidy)?;

        // The deferred pool contribution is checked in `miner_fees_are_valid()` according to
        // [ZIP-1015](https://zips.z.cash/zip-1015).
        let mut deferred_pool_balance_change = funding_streams
            .remove(&FundingStreamReceiver::Deferred)
            .unwrap_or_default()
            .constrain::<NegativeAllowed>()?;

        // Check the one-time lockbox disbursements in the NU6.1 activation block's coinbase tx
        // according to [ZIP-271] and [ZIP-1016].
        //
        // [ZIP-271]: <https://zips.z.cash/zip-0271>
        // [ZIP-1016]: <https://zips.z.cash/zip-101>
        if Some(height) == NetworkUpgrade::Nu6_1.activation_height(net) {
            let lockbox_disbursements = net.lockbox_disbursements(height);

            // The Mainnet and default Testnet disbursement lists are hardcoded and must be
            // non-empty. Custom testnets and Regtest may configure no disbursements, in which
            // case the NU6.1 activation block is not required to contain any disbursement
            // outputs.
            let must_have_disbursements =
                matches!(net, Network::Mainnet) || net.is_default_testnet();
            if lockbox_disbursements.is_empty() && must_have_disbursements {
                Err(CoinbaseTransactionError::MissingLockboxDisbursements)?;
            }

            deferred_pool_balance_change = lockbox_disbursements.into_iter().try_fold(
                deferred_pool_balance_change,
                |balance, (addr, expected_amount)| {
                    if !has_amount(&addr, expected_amount) {
                        Err(SubsidyError::OneTimeLockboxDisbursementNotFound)?;
                    }

                    balance
                        .checked_sub(expected_amount)
                        .ok_or(SubsidyError::Underflow)
                },
            )?;
        };

        // Check each funding stream output.
        funding_streams.into_iter().try_for_each(
            |(receiver, expected_amount)| -> Result<(), CoinbaseTransactionError> {
                let addr = funding_stream_address(height, net, receiver)
                    .ok_or(CoinbaseTransactionError::FundingStreamAddressNotFound)?;

                if !has_amount(addr, expected_amount) {
                    Err(SubsidyError::FundingStreamNotFound)?;
                }

                Ok(())
            },
        )?;

        Ok(DeferredPoolBalanceChange::new(deferred_pool_balance_change))
    }
}

/// Returns `Ok(())` if the miner fees consensus rule is valid.
///
/// [7.1.2]: https://zips.z.cash/protocol/protocol.pdf#txnconsensus
pub fn miner_fees_are_valid(
    coinbase_tx: &Transaction,
    height: Height,
    block_miner_fees: Amount<NonNegative>,
    expected_block_subsidy: Amount<NonNegative>,
    expected_deferred_pool_balance_change: DeferredPoolBalanceChange,
    network: &Network,
) -> Result<(), CoinbaseTransactionError> {
    let transparent_value_balance = coinbase_tx
        .outputs()
        .iter()
        .map(|output| output.value())
        .sum::<Result<Amount<NonNegative>, AmountError>>()
        .map_err(|_| SubsidyError::Overflow)?
        .constrain()
        .map_err(CoinbaseTransactionError::InvalidTransparentValueBalance)?;
    let sapling_value_balance = coinbase_tx.sapling_value_balance().sapling_amount();
    let orchard_value_balance = coinbase_tx.orchard_value_balance().orchard_amount();
    // [NU6.3 onward] The Ironwood pool is shielded too, so its value balance affects the coinbase
    // output value exactly like Sapling and Orchard. This is zero for pre-v6 coinbase transactions
    // (no Ironwood bundle), so it is a no-op before NU6.3.
    let ironwood_value_balance = coinbase_tx.ironwood_value_balance().ironwood_amount();

    // # Consensus
    //
    // > - define the total output value of its coinbase transaction to be the total value in zatoshi of its transparent
    // >   outputs, minus vbalanceSapling, minus vbalanceOrchard, minus vbalanceIronwood, plus totalDeferredOutput(height);
    // > – define the total input value of its coinbase transaction to be the value in zatoshi of the block subsidy,
    // >   plus the transaction fees paid by transactions in the block.
    //
    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    //
    // The expected lockbox funding stream output of the coinbase transaction is also subtracted
    // from the block subsidy value plus the transaction fees paid by transactions in this block.
    let total_output_value = (transparent_value_balance
        - sapling_value_balance
        - orchard_value_balance
        - ironwood_value_balance
        + expected_deferred_pool_balance_change.value())
    .map_err(|_| SubsidyError::Overflow)?;

    // # Consensus
    //
    // > For every block from NU7 activation onward, the coinbase transaction MUST be balanced using
    // > MinerFees in place of TransactionFees
    //
    // where `MinerFees(height) := TransactionFees(height) - NSMFeeContribution(height)`.
    //
    // https://github.com/zcash/zips/pull/1363
    let claimable_miner_fees = (block_miner_fees
        - nsm_fee_contribution(height, network, block_miner_fees))
    .map_err(|_| SubsidyError::Overflow)?;

    let total_input_value =
        (expected_block_subsidy + claimable_miner_fees).map_err(|_| SubsidyError::Overflow)?;

    // # Consensus
    //
    // > [Pre-NU6] The total output of a coinbase transaction MUST NOT be greater than its total
    // input.
    //
    // > [NU6 onward] The total output of a coinbase transaction MUST be equal to its total input.
    if if NetworkUpgrade::current(network, height) < NetworkUpgrade::Nu6 {
        total_output_value > total_input_value
    } else {
        total_output_value != total_input_value
    } {
        Err(SubsidyError::InvalidMinerFees)?
    };

    Ok(())
}
