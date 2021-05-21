use proptest::{
    arbitrary::{any, Arbitrary},
    prelude::*,
};

use std::sync::Arc;

use crate::{
    parameters::{Network, NetworkUpgrade},
    primitives,
    work::{difficulty::CompactDifficulty, equihash},
};

use super::*;

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
/// The configuration data for proptest when generating arbitrary chains
pub struct LedgerState {
    /// The tip height of the block or start of the chain.
    ///
    /// To get the network upgrade, use the `network_upgrade` method.
    pub tip_height: Height,

    /// The network to generate fake blocks for.
    pub network: Network,

    /// Overrides the network upgrade calculated from `tip_height` and `network`.
    ///
    /// To get the network upgrade, use the `network_upgrade` method.
    pub network_upgrade_override: Option<NetworkUpgrade>,

    /// Generate coinbase transactions.
    ///
    /// In a block or transaction vector, make the first transaction a coinbase
    /// transaction.
    ///
    /// For an individual transaction, make the transaction a coinbase
    /// transaction.
    pub(crate) has_coinbase: bool,
}

impl LedgerState {
    /// Returns the network upgrade for this ledger state.
    ///
    /// If `network_upgrade_override` is set, it replaces the upgrade calculated
    /// using `tip_height` and `network`.
    pub fn network_upgrade(&self) -> NetworkUpgrade {
        if let Some(network_upgrade_override) = self.network_upgrade_override {
            network_upgrade_override
        } else {
            NetworkUpgrade::current(self.network, self.tip_height)
        }
    }

    /// Returns a strategy for creating `LedgerState`s that always have coinbase
    /// transactions.
    pub fn coinbase_strategy() -> BoxedStrategy<Self> {
        Self::arbitrary_with(true)
    }
}

impl Default for LedgerState {
    fn default() -> Self {
        let network = Network::Mainnet;
        let most_recent_nu = NetworkUpgrade::current(network, Height::MAX);
        let most_recent_activation_height = most_recent_nu.activation_height(network).unwrap();

        // TODO: dynamically select any future network upgrade (#1974)
        let nu5_activation_height = NetworkUpgrade::Nu5.activation_height(network);
        let nu5_override = if nu5_activation_height.is_some() {
            None
        } else {
            Some(NetworkUpgrade::Nu5)
        };

        Self {
            tip_height: most_recent_activation_height,
            network,
            network_upgrade_override: nu5_override,
            has_coinbase: true,
        }
    }
}

impl Arbitrary for LedgerState {
    type Parameters = bool;

    /// Generate an arbitrary `LedgerState`.
    ///
    /// The default strategy arbitrarily skips some coinbase transactions. To
    /// override, use `LedgerState::coinbase_strategy`.
    fn arbitrary_with(require_coinbase: Self::Parameters) -> Self::Strategy {
        (
            any::<Height>(),
            any::<Network>(),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(move |(tip_height, network, nu5_override, has_coinbase)| {
                // TODO: dynamically select any future network upgrade (#1974)
                let network_upgrade_override = if nu5_override {
                    Some(NetworkUpgrade::Nu5)
                } else {
                    None
                };

                LedgerState {
                    tip_height,
                    network,
                    network_upgrade_override,
                    has_coinbase: require_coinbase || has_coinbase,
                }
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Block {
    type Parameters = LedgerState;

    fn arbitrary_with(ledger_state: Self::Parameters) -> Self::Strategy {
        let transactions_strategy = Transaction::vec_strategy(ledger_state, 2);

        (any::<Header>(), transactions_strategy)
            .prop_map(|(header, transactions)| Self {
                header,
                transactions,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Block {
    /// Returns a strategy for creating Vecs of blocks with increasing height of
    /// the given length.
    pub fn partial_chain_strategy(
        init: LedgerState,
        count: usize,
    ) -> BoxedStrategy<Vec<Arc<Self>>> {
        let mut current = init;
        let mut vec = Vec::with_capacity(count);
        for _ in 0..count {
            vec.push(Block::arbitrary_with(current).prop_map(Arc::new));
            current.tip_height.0 += 1;
        }

        vec.boxed()
    }
}

impl Arbitrary for Commitment {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (any::<[u8; 32]>(), any::<Network>(), any::<Height>())
            .prop_map(|(commitment_bytes, network, block_height)| {
                match Commitment::from_bytes(commitment_bytes, network, block_height) {
                    Ok(commitment) => commitment,
                    // just fix up the reserved values when they fail
                    Err(_) => Commitment::from_bytes(
                        super::commitment::RESERVED_BYTES,
                        network,
                        block_height,
                    )
                    .expect("from_bytes only fails due to reserved bytes"),
                }
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Header {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (
            // version is interpreted as i32 in the spec, so we are limited to i32::MAX here
            (4u32..(i32::MAX as u32)),
            any::<Hash>(),
            any::<merkle::Root>(),
            any::<[u8; 32]>(),
            primitives::arbitrary::datetime_u32(),
            any::<CompactDifficulty>(),
            any::<[u8; 32]>(),
            any::<equihash::Solution>(),
        )
            .prop_map(
                |(
                    version,
                    previous_block_hash,
                    merkle_root,
                    commitment_bytes,
                    time,
                    difficulty_threshold,
                    nonce,
                    solution,
                )| Header {
                    version,
                    previous_block_hash,
                    merkle_root,
                    commitment_bytes,
                    time,
                    difficulty_threshold,
                    nonce,
                    solution,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
