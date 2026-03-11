//! Initialization and arbitrary data generation functions for zebra-state.

use std::sync::Arc;

use proptest::prelude::*;

use zebra_chain::{
    block::{Block, Height},
    parameters::{
        Network::{self, *},
        NetworkUpgrade,
    },
    serialization::ZcashDeserializeInto,
    transaction::{LockTime, Transaction},
};

use crate::{
    service::{
        check, finalized_state::FinalizedState, non_finalized_state::NonFinalizedState, read,
    },
    CheckpointVerifiedBlock, Config,
};

/// Generate a chain that allows us to make tests for the legacy chain rules.
///
/// Arguments:
/// - `transaction_version_override`: See `LedgerState::height_strategy` for details.
/// - `transaction_has_valid_network_upgrade`: See `LedgerState::height_strategy` for details.
///   Note: `false` allows zero or more invalid network upgrades.
/// - `blocks_after_nu_activation`: The number of blocks the strategy will generate
///   after the provided `network_upgrade`.
/// - `network_upgrade` - The network upgrade that we are using to simulate from where the
///   legacy chain checks should start to apply.
///
/// Returns:
/// A generated arbitrary strategy for the provided arguments.
pub(crate) fn partial_nu5_chain_strategy(
    transaction_version_override: u32,
    transaction_has_valid_network_upgrade: bool,
    blocks_after_nu_activation: u32,
    // TODO: This argument can be removed and just use Nu5 after we have an activation height #1841
    network_upgrade: NetworkUpgrade,
) -> impl Strategy<
    Value = (
        Network,
        Height,
        zebra_chain::fmt::SummaryDebug<Vec<Arc<Block>>>,
    ),
> {
    (
        any::<Network>(),
        NetworkUpgrade::reduced_branch_id_strategy(),
    )
        .prop_flat_map(move |(network, random_nu)| {
            // TODO: update this to Nu5 after we have a height #1841
            let mut nu = network_upgrade;
            let nu_activation = nu.activation_height(&network).unwrap();
            let height = Height(nu_activation.0 + blocks_after_nu_activation);

            // The `network_upgrade_override` will not be enough as when it is `None`,
            // current network upgrade will be used (`NetworkUpgrade::Canopy`) which will be valid.
            if !transaction_has_valid_network_upgrade {
                nu = random_nu;
            }

            zebra_chain::block::LedgerState::height_strategy(
                height,
                Some(nu),
                Some(transaction_version_override),
                transaction_has_valid_network_upgrade,
            )
            .prop_flat_map(move |init| {
                Block::partial_chain_strategy(
                    init,
                    blocks_after_nu_activation as usize,
                    check::utxo::transparent_coinbase_spend,
                    false,
                )
            })
            .prop_map(move |partial_chain| {
                let network_clone = network.clone();
                (network_clone, nu_activation, partial_chain)
            })
        })
}

/// Return a new `StateService` containing the mainnet genesis block.
/// Also returns the finalized genesis block itself.
pub(crate) fn new_state_with_mainnet_genesis(
) -> (FinalizedState, NonFinalizedState, CheckpointVerifiedBlock) {
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block should deserialize");

    let config = Config::ephemeral();
    let network = Mainnet;

    let mut finalized_state = FinalizedState::new_with_debug(
        &config,
        &network,
        // The tests that use this setup function also commit invalid blocks to the state.
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    );
    let non_finalized_state = NonFinalizedState::new(&network);

    assert_eq!(
        None,
        read::best_tip(&non_finalized_state, &finalized_state.db)
    );

    let genesis = CheckpointVerifiedBlock::from(genesis);
    finalized_state
        .commit_finalized_direct(genesis.clone().into(), None, "test")
        .expect("unexpected invalid genesis block test vector");

    assert_eq!(
        Some((Height(0), genesis.hash)),
        read::best_tip(&non_finalized_state, &finalized_state.db)
    );

    (finalized_state, non_finalized_state, genesis)
}

/// Return a `Transaction::V4` with the coinbase data from `coinbase`.
///
/// Used to convert a coinbase transaction to a version that the non-finalized state will accept.
pub(crate) fn transaction_v4_from_coinbase(coinbase: &Transaction) -> Transaction {
    assert!(
        !coinbase.has_sapling_shielded_data(),
        "conversion assumes sapling shielded data is None"
    );

    Transaction::V4 {
        inputs: coinbase.inputs().to_vec(),
        outputs: coinbase.outputs().to_vec(),
        lock_time: coinbase.lock_time().unwrap_or_else(LockTime::unlocked),
        // `Height(0)` means that the expiry height is ignored
        expiry_height: coinbase.expiry_height().unwrap_or(Height(0)),
        // invalid for coinbase transactions
        joinsplit_data: None,
        sapling_shielded_data: None,
    }
}
