use std::sync::Arc;

use proptest::{
    num::usize::BinarySearch,
    prelude::*,
    strategy::{NewTree, ValueTree},
    test_runner::TestRunner,
};

use zebra_chain::{block::Block, fmt::SummaryDebug, parameters::NetworkUpgrade, LedgerState};
#[cfg(test)]
use zebra_chain::{block::Height, parameters::Network::*, serialization::ZcashDeserializeInto};

use crate::arbitrary::Prepare;

use super::*;

const MAX_PARTIAL_CHAIN_BLOCKS: usize = 102;

#[derive(Debug)]
pub struct PreparedChainTree {
    chain: Arc<SummaryDebug<Vec<PreparedBlock>>>,
    count: BinarySearch,
    network: Network,
}

impl ValueTree for PreparedChainTree {
    type Value = (
        Arc<SummaryDebug<Vec<PreparedBlock>>>,
        <BinarySearch as ValueTree>::Value,
        Network,
    );

    fn current(&self) -> Self::Value {
        (self.chain.clone(), self.count.current(), self.network)
    }

    fn simplify(&mut self) -> bool {
        self.count.simplify()
    }

    fn complicate(&mut self) -> bool {
        self.count.complicate()
    }
}

#[derive(Debug, Default)]
pub struct PreparedChain {
    // the proptests are threaded (not async), so we want to use a threaded mutex here
    chain: std::sync::Mutex<Option<(Network, Arc<SummaryDebug<Vec<PreparedBlock>>>)>>,
}

impl Strategy for PreparedChain {
    type Tree = PreparedChainTree;
    type Value = <PreparedChainTree as ValueTree>::Value;

    fn new_tree(&self, runner: &mut TestRunner) -> NewTree<Self> {
        let mut chain = self.chain.lock().unwrap();
        if chain.is_none() {
            // TODO: use the latest network upgrade (#1974)
            let ledger_strategy = LedgerState::genesis_strategy(NetworkUpgrade::Nu5, None, false);

            let (network, blocks) = ledger_strategy
                .prop_flat_map(|ledger| {
                    (
                        Just(ledger.network),
                        Block::partial_chain_strategy(ledger, MAX_PARTIAL_CHAIN_BLOCKS),
                    )
                })
                .prop_map(|(network, vec)| {
                    (
                        network,
                        vec.iter()
                            .map(|blk| blk.clone().prepare())
                            .collect::<Vec<_>>(),
                    )
                })
                .new_tree(runner)?
                .current();
            *chain = Some((network, Arc::new(SummaryDebug(blocks))));
        }

        let chain = chain.clone().expect("should be generated");
        let count = (1..chain.1.len()).new_tree(runner)?;
        Ok(PreparedChainTree {
            chain: chain.1,
            count,
            network: chain.0,
        })
    }
}

/// Generate a chain that allows us to make tests for the legacy chain rules.
///
/// Arguments:
/// - `transaction_version_override`: See `LedgerState::height_strategy` for details.
/// - `transaction_has_valid_network_upgrade`: See `LedgerState::height_strategy` for details.
///    Note: `false` allows zero or more invalid network upgrades.
/// - `blocks_after_nu_activation`: The number of blocks the strategy will generate
/// after the provided `network_upgrade`.
/// - `network_upgrade` - The network upgrade that we are using to simulate from where the
/// legacy chain checks should start to apply.
///
/// Returns:
/// A generated arbitrary strategy for the provided arguments.
#[cfg(test)]
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
            let nu_activation = nu.activation_height(network).unwrap();
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
                Block::partial_chain_strategy(init, blocks_after_nu_activation as usize)
            })
            .prop_map(move |partial_chain| (network, nu_activation, partial_chain))
        })
}

/// Return a new `StateService` containing the mainnet genesis block.
/// Also returns the finalized genesis block itself.
#[cfg(test)]
pub(super) fn new_state_with_mainnet_genesis() -> (StateService, FinalizedBlock) {
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block should deserialize");

    let mut state = StateService::new(Config::ephemeral(), Mainnet);

    assert_eq!(None, state.best_tip());

    let genesis = FinalizedBlock::from(genesis);
    state
        .disk
        .commit_finalized_direct(genesis.clone(), "test")
        .expect("unexpected invalid genesis block test vector");

    assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());

    (state, genesis)
}

/// Return a `Transaction::V4` with the coinbase data from `coinbase`.
///
/// Used to convert a coinbase transaction to a version that the non-finalized state will accept.
#[cfg(test)]
pub(super) fn transaction_v4_from_coinbase(coinbase: &Transaction) -> Transaction {
    assert!(
        !coinbase.has_sapling_shielded_data(),
        "conversion assumes sapling shielded data is None"
    );

    Transaction::V4 {
        inputs: coinbase.inputs().to_vec(),
        outputs: coinbase.outputs().to_vec(),
        lock_time: coinbase.lock_time(),
        // `Height(0)` means that the expiry height is ignored
        expiry_height: coinbase.expiry_height().unwrap_or(Height(0)),
        // invalid for coinbase transactions
        joinsplit_data: None,
        sapling_shielded_data: None,
    }
}
