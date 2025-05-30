//! Randomised property tests for transaction verification.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use chrono::{DateTime, Duration, Utc};
use proptest::{collection::vec, prelude::*};
use tower::{buffer::Buffer, ServiceExt};

use zebra_chain::{
    amount::Amount,
    block,
    parameters::{Network, NetworkUpgrade},
    serialization::arbitrary::{datetime_full, datetime_u32},
    transaction::{LockTime, Transaction},
    transparent,
};

use crate::{error::TransactionError, transaction};

use super::mock_transparent_transfer;

/// The maximum number of transparent inputs to include in a mock transaction.
const MAX_TRANSPARENT_INPUTS: usize = 10;

proptest! {
    /// Test if a transaction that has a zero value as the lock time is always unlocked.
    #[test]
    fn zero_lock_time_is_always_unlocked(
        (network, block_height) in sapling_onwards_strategy(),
        block_time in datetime_full(),
        relative_source_fund_heights in vec(0.0..1.0, 1..=MAX_TRANSPARENT_INPUTS),
        transaction_version in 4_u8..=5,
    ) {
        let _init_guard = zebra_test::init();

        let zero_lock_time = LockTime::Height(block::Height(0));

        let (transaction, known_utxos) = mock_transparent_transaction(
            &network,
            block_height,
            relative_source_fund_heights,
            transaction_version,
            zero_lock_time,
        );

        let transaction_id = transaction.unmined_id();

        let result = validate(transaction, block_height, block_time, known_utxos, network);

        prop_assert!(
            result.is_ok(),
            "Unexpected validation error: {}",
            result.unwrap_err()
        );
        prop_assert_eq!(result.unwrap().tx_id(), transaction_id);
    }

    /// Test if having [`u32::MAX`] as the sequence number of all inputs disables the lock time.
    #[test]
    fn lock_time_is_ignored_because_of_sequence_numbers(
        (network, block_height) in sapling_onwards_strategy(),
        block_time in datetime_full(),
        relative_source_fund_heights in vec(0.0..1.0, 1..=MAX_TRANSPARENT_INPUTS),
        transaction_version in 4_u8..=5,
        lock_time in any::<LockTime>(),
    ) {
        let _init_guard = zebra_test::init();

        let (mut transaction, known_utxos) = mock_transparent_transaction(
            &network,
            block_height,
            relative_source_fund_heights,
            transaction_version,
            lock_time,
        );

        for input in transaction.inputs_mut() {
            input.set_sequence(u32::MAX);
        }

        let transaction_id = transaction.unmined_id();

        let result = validate(transaction, block_height, block_time, known_utxos, network);

        prop_assert!(
            result.is_ok(),
            "Unexpected validation error: {}",
            result.unwrap_err()
        );
        prop_assert_eq!(result.unwrap().tx_id(), transaction_id);
    }

    /// Test if a transaction locked at a certain block height is rejected.
    #[test]
    fn transaction_is_rejected_based_on_lock_height(
        (network, block_height) in sapling_onwards_strategy(),
        block_time in datetime_full(),
        relative_source_fund_heights in vec(0.0..1.0, 1..=MAX_TRANSPARENT_INPUTS),
        transaction_version in 4_u8..=5,
        relative_unlock_height in 0.0..1.0,
    ) {
        let _init_guard = zebra_test::init();

        let unlock_height = scale_block_height(block_height, None, relative_unlock_height);
        let lock_time = LockTime::Height(unlock_height);

        let (transaction, known_utxos) = mock_transparent_transaction(
            &network,
            block_height,
            relative_source_fund_heights,
            transaction_version,
            lock_time,
        );

        let result = validate(transaction, block_height, block_time, known_utxos, network);

        prop_assert_eq!(
            result,
            Err(TransactionError::LockedUntilAfterBlockHeight(unlock_height))
        );
    }

    /// Test if a transaction locked at a certain block time is rejected.
    #[test]
    fn transaction_is_rejected_based_on_lock_time(
        (network, block_height) in sapling_onwards_strategy(),
        first_datetime in datetime_u32(),
        second_datetime in datetime_u32(),
        relative_source_fund_heights in vec(0.0..1.0, 1..=MAX_TRANSPARENT_INPUTS),
        transaction_version in 4_u8..=5,
    ) {
        let _init_guard = zebra_test::init();

        let (unlock_time, block_time) = if first_datetime >= second_datetime {
            (first_datetime, second_datetime)
        } else {
            (second_datetime, first_datetime)
        };

        let (transaction, known_utxos) = mock_transparent_transaction(
            &network,
            block_height,
            relative_source_fund_heights,
            transaction_version,
            LockTime::Time(unlock_time),
        );

        let result = validate(transaction, block_height, block_time, known_utxos, network);

        prop_assert_eq!(
            result,
            Err(TransactionError::LockedUntilAfterBlockTime(unlock_time))
        );
    }

    /// Test if a transaction unlocked at an earlier block time is accepted.
    #[test]
    fn transaction_with_lock_height_is_accepted(
        (network, block_height) in sapling_onwards_strategy(),
        block_time in datetime_full(),
        relative_source_fund_heights in vec(0.0..1.0, 1..=MAX_TRANSPARENT_INPUTS),
        transaction_version in 4_u8..=5,
        relative_unlock_height in 0.0..1.0,
    ) {
        let _init_guard = zebra_test::init();

        // Because `scale_block_height` uses the range `[min, max)`, with `max` being
        // non-inclusive, we have to use `block_height + 1` as the upper bound in order to test
        // verifying at a block height equal to the lock height.
        let exclusive_max_height = block::Height(block_height.0 + 1);
        let unlock_height = scale_block_height(None, exclusive_max_height, relative_unlock_height);
        let lock_time = LockTime::Height(unlock_height);

        let (transaction, known_utxos) = mock_transparent_transaction(
            &network,
            block_height,
            relative_source_fund_heights,
            transaction_version,
            lock_time,
        );

        let transaction_id = transaction.unmined_id();

        let result = validate(transaction, block_height, block_time, known_utxos, network);

        prop_assert!(
            result.is_ok(),
            "Unexpected validation error: {}",
            result.unwrap_err()
        );
        prop_assert_eq!(result.unwrap().tx_id(), transaction_id);
    }

    /// Test if transaction unlocked at a previous block time is accepted.
    #[test]
    fn transaction_with_lock_time_is_accepted(
        (network, block_height) in sapling_onwards_strategy(),
        first_datetime in datetime_u32(),
        second_datetime in datetime_u32(),
        relative_source_fund_heights in vec(0.0..1.0, 1..=MAX_TRANSPARENT_INPUTS),
        transaction_version in 4_u8..=5,
    ) {
        let _init_guard = zebra_test::init();

        let (unlock_time, block_time) = if first_datetime < second_datetime {
            (first_datetime, second_datetime)
        } else if first_datetime > second_datetime {
            (second_datetime, first_datetime)
        } else if first_datetime == DateTime::<Utc>::MAX_UTC {
            (first_datetime - Duration::nanoseconds(1), first_datetime)
        } else {
            (first_datetime, first_datetime + Duration::nanoseconds(1))
        };

        let (transaction, known_utxos) = mock_transparent_transaction(
            &network,
            block_height,
            relative_source_fund_heights,
            transaction_version,
            LockTime::Time(unlock_time),
        );

        let transaction_id = transaction.unmined_id();

        let result = validate(transaction, block_height, block_time, known_utxos, network);

        prop_assert!(
            result.is_ok(),
            "Unexpected validation error: {}",
            result.unwrap_err()
        );
        prop_assert_eq!(result.unwrap().tx_id(), transaction_id);
    }
}

/// Generates an arbitrary [`block::Height`] after the Sapling activation height
/// on an arbitrary network.
///
/// A proptest [`Strategy`] that generates random tuples with:
///
/// - a network (mainnet or testnet);
/// - a block height between the Sapling activation height (inclusive) on that
///   network and the maximum transaction expiry height.
fn sapling_onwards_strategy() -> impl Strategy<Value = (Network, block::Height)> {
    any::<Network>().prop_flat_map(|network| {
        let start_height_value = NetworkUpgrade::Sapling
            .activation_height(&network)
            .expect("Sapling to have an activation height")
            .0;

        let end_height_value = block::Height::MAX_EXPIRY_HEIGHT.0;

        (start_height_value..=end_height_value)
            .prop_map(move |height_value| (network.clone(), block::Height(height_value)))
    })
}

/// Create a mock transaction that only transfers transparent amounts.
///
/// # Parameters
///
/// - `network`: the network to use for the transaction (mainnet or testnet)
/// - `block_height`: the block height to be used for the transaction's expiry height as well as
///   the height that the transaction was (hypothetically) included in a block
/// - `relative_source_heights`: a list of values in the range `0.0..1.0`; each item results in the
///   creation of a transparent input and output, where the item itself represents a scaled value
///   to be converted into a block height between zero and `block_height` (see
///   [`scale_block_height`] for details) to serve as the block height that created the input UTXO
/// - `transaction_version`: a value that's either `4` or `5` indicating the transaction version to
///   be generated; this value is sanitized by [`sanitize_transaction_version`], so it may not be
///   able to create a V5 transaction if the `block_height` is before the NU5 activation height
/// - `lock_time`: the transaction lock time to be used (note that all transparent inputs have a
///   sequence number of `0`, so the lock time is enabled by default)
///
/// # Panics
///
/// - if `transaction_version` is not `4` or `5` (the only transaction versions that are currently
///   supported by the transaction verifier)
/// - if `relative_source_heights` has more than `u32::MAX` items (see
///   [`mock_transparent_transfers`] for details)
/// - if any item of `relative_source_heights` is not in the range `0.0..1.0` (see
///   [`scale_block_height`] for details)
fn mock_transparent_transaction(
    network: &Network,
    block_height: block::Height,
    relative_source_heights: Vec<f64>,
    transaction_version: u8,
    lock_time: LockTime,
) -> (
    Transaction,
    HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
) {
    let (transaction_version, network_upgrade) =
        sanitize_transaction_version(network, transaction_version, block_height);

    // Create fake transparent transfers that should succeed
    let (inputs, outputs, known_utxos) =
        mock_transparent_transfers(relative_source_heights, block_height);

    // Create the mock transaction
    let expiry_height = block_height;

    let transaction = match transaction_version {
        4 => Transaction::V4 {
            inputs,
            outputs,
            lock_time,
            expiry_height,
            joinsplit_data: None,
            sapling_shielded_data: None,
        },
        5 => Transaction::V5 {
            inputs,
            outputs,
            lock_time,
            expiry_height,
            sapling_shielded_data: None,
            orchard_shielded_data: None,
            network_upgrade,
        },
        invalid_version => unreachable!("invalid transaction version: {}", invalid_version),
    };

    (transaction, known_utxos)
}

/// Sanitize a transaction version so that it is supported at the specified `block_height` of the
/// `network`.
///
/// The `transaction_version` might be reduced if it is not supported by the network upgrade active
/// at the `block_height` of the specified `network`.
fn sanitize_transaction_version(
    network: &Network,
    transaction_version: u8,
    block_height: block::Height,
) -> (u8, NetworkUpgrade) {
    let network_upgrade = NetworkUpgrade::current(network, block_height);

    let max_version = {
        use NetworkUpgrade::*;

        match network_upgrade {
            Genesis => 1,
            BeforeOverwinter => 2,
            Overwinter => 3,
            Sapling | Blossom | Heartwood | Canopy => 4,
            // FIXME: Use 6 for Nu7
            Nu5 | Nu6 | Nu6_1 | Nu7 => 5,
        }
    };

    let sanitized_version = transaction_version.min(max_version);

    (sanitized_version, network_upgrade)
}

/// Create multiple mock transparent transfers.
///
/// Creates one mock transparent transfer per item in the `relative_source_heights` vector. Each
/// item represents a relative scale (in the range `0.0..1.0`) representing the scale to obtain a
/// block height between the genesis block and the specified `block_height`. Each block height is
/// then used as the height for the source of the UTXO that will be spent by the transfer.
///
/// The function returns a list of inputs and outputs to be included in a mock transaction, as well
/// as a [`HashMap`] of source UTXOs to be sent to the transaction verifier.
///
/// # Panics
///
/// This will panic if there are more than [`u32::MAX`] items in `relative_source_heights`. Ideally
/// the tests should use a number of items at most [`MAX_TRANSPARENT_INPUTS`].
fn mock_transparent_transfers(
    relative_source_heights: Vec<f64>,
    block_height: block::Height,
) -> (
    Vec<transparent::Input>,
    Vec<transparent::Output>,
    HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
) {
    let transfer_count = relative_source_heights.len();
    let mut inputs = Vec::with_capacity(transfer_count);
    let mut outputs = Vec::with_capacity(transfer_count);
    let mut known_utxos = HashMap::with_capacity(transfer_count);

    for (index, relative_source_height) in relative_source_heights.into_iter().enumerate() {
        let fake_source_fund_height =
            scale_block_height(None, block_height, relative_source_height);

        let outpoint_index = index
            .try_into()
            .expect("too many mock transparent transfers requested");

        let (input, output, new_utxos) = mock_transparent_transfer(
            fake_source_fund_height,
            true,
            outpoint_index,
            Amount::try_from(1).expect("invalid value"),
        );

        inputs.push(input);
        outputs.push(output);
        known_utxos.extend(new_utxos);
    }

    (inputs, outputs, known_utxos)
}

/// Selects a [`block::Height`] between `min_height` and `max_height` using the `scale` factor.
///
/// The `scale` must be in the range `0.0..1.0`, where `0.0` results in the selection of
/// `min_height` and `1.0` would select the `max_height` if the range was inclusive. The range is
/// exclusive however, so `max_height` is never selected (unless it is equal to `min_height`).
///
/// # Panics
///
/// - if `scale` is not in the range `0.0..1.0`
/// - if `min_height` is greater than `max_height`
fn scale_block_height(
    min_height: impl Into<Option<block::Height>>,
    max_height: impl Into<Option<block::Height>>,
    scale: f64,
) -> block::Height {
    assert!(scale >= 0.0);
    assert!(scale < 1.0);

    let min_height = min_height.into().unwrap_or(block::Height(0));
    let max_height = max_height.into().unwrap_or(block::Height::MAX);

    assert!(min_height <= max_height);

    let min_height_value = f64::from(min_height.0);
    let max_height_value = f64::from(max_height.0);
    let height_range = max_height_value - min_height_value;

    let new_height_value = (height_range * scale + min_height_value).floor();

    block::Height(new_height_value as u32)
}

/// Validate a `transaction` using a [`transaction::Verifier`] and return the result.
///
/// Configures an asynchronous runtime to run the verifier, sets it up and then uses it verify a
/// `transaction` using the provided parameters.
fn validate(
    transaction: Transaction,
    height: block::Height,
    block_time: DateTime<Utc>,
    known_utxos: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    network: Network,
) -> Result<transaction::Response, TransactionError> {
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        // Initialize the verifier
        let state_service =
            tower::service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = transaction::Verifier::new_for_tests(&network, state_service);
        let verifier = Buffer::new(verifier, 10);
        let transaction_hash = transaction.hash();

        // Test the transaction verifier
        verifier
            .clone()
            .oneshot(transaction::Request::Block {
                transaction_hash,
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(known_utxos),
                known_outpoint_hashes: Arc::new(HashSet::new()),
                height,
                time: block_time,
            })
            .await
            .map_err(|err| {
                *err.downcast()
                    .expect("error type should be TransactionError")
            })
    })
}
