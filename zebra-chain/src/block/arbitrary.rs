use proptest::{
    arbitrary::{any, Arbitrary},
    prelude::*,
};

use std::{collections::HashSet, convert::TryInto, sync::Arc};

use crate::{
    block,
    fmt::SummaryDebug,
    orchard,
    parameters::{
        Network,
        NetworkUpgrade::{self, *},
        GENESIS_PREVIOUS_BLOCK_HASH,
    },
    serialization,
    transparent::Input::*,
    work::{difficulty::CompactDifficulty, equihash},
};

use super::*;

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
/// The configuration data for proptest when generating arbitrary chains
pub struct LedgerState {
    /// The height of the generated block, or the start height of the generated chain.
    ///
    /// To get the network upgrade, use the `network_upgrade` method.
    ///
    /// If `network_upgrade_override` is not set, the network upgrade is derived
    /// from the `height` and `network`.
    pub height: Height,

    /// The network to generate fake blocks for.
    pub network: Network,

    /// Overrides the network upgrade calculated from `height` and `network`.
    ///
    /// To get the network upgrade, use the `network_upgrade` method.
    network_upgrade_override: Option<NetworkUpgrade>,

    /// Overrides the previous block hashes in blocks generated by this ledger.
    previous_block_hash_override: Option<block::Hash>,

    /// Regardless of tip height and network, every transaction is this version.
    transaction_version_override: Option<u32>,

    /// Every V5 and later transaction has a valid `network_upgrade` field.
    ///
    /// If `false`, zero or more transactions may have invalid network upgrades.
    transaction_has_valid_network_upgrade: bool,

    /// Generate coinbase transactions.
    ///
    /// In a block or transaction vector, make the first transaction a coinbase
    /// transaction.
    ///
    /// For an individual transaction, make the transaction a coinbase
    /// transaction.
    pub(crate) has_coinbase: bool,
}

/// Overrides for arbitrary [`LedgerState`]s.
#[derive(Debug, Clone, Copy)]
pub struct LedgerStateOverride {
    /// Every chain starts at this block. Single blocks have this height.
    pub height_override: Option<Height>,

    /// Every chain starts with a block with this previous block hash.
    /// Single blocks have this previous block hash.
    pub previous_block_hash_override: Option<block::Hash>,

    /// Regardless of tip height and network, every block has features from this
    /// network upgrade.
    pub network_upgrade_override: Option<NetworkUpgrade>,

    /// Regardless of tip height and network, every transaction is this version.
    pub transaction_version_override: Option<u32>,

    /// Every V5 and later transaction has a valid `network_upgrade` field.
    ///
    /// If `false`, zero or more transactions may have invalid network upgrades.
    pub transaction_has_valid_network_upgrade: bool,

    /// Every block has exactly one coinbase transaction.
    /// Transactions are always coinbase transactions.
    pub always_has_coinbase: bool,
}

impl LedgerState {
    /// Returns the default strategy for creating arbitrary `LedgerState`s.
    pub fn default_strategy() -> BoxedStrategy<Self> {
        Self::arbitrary_with(LedgerStateOverride::default())
    }

    /// Returns a strategy for creating arbitrary `LedgerState`s, without any
    /// overrides.
    pub fn no_override_strategy() -> BoxedStrategy<Self> {
        Self::arbitrary_with(LedgerStateOverride {
            height_override: None,
            previous_block_hash_override: None,
            network_upgrade_override: None,
            transaction_version_override: None,
            transaction_has_valid_network_upgrade: false,
            always_has_coinbase: false,
        })
    }

    /// Returns a strategy for creating `LedgerState`s with features from
    /// `network_upgrade_override`.
    ///
    /// These featues ignore the actual tip height and network.
    pub fn network_upgrade_strategy(
        network_upgrade_override: NetworkUpgrade,
        transaction_version_override: impl Into<Option<u32>>,
        transaction_has_valid_network_upgrade: bool,
    ) -> BoxedStrategy<Self> {
        Self::arbitrary_with(LedgerStateOverride {
            height_override: None,
            previous_block_hash_override: None,
            network_upgrade_override: Some(network_upgrade_override),
            transaction_version_override: transaction_version_override.into(),
            transaction_has_valid_network_upgrade,
            always_has_coinbase: false,
        })
    }

    /// Returns a strategy for creating `LedgerState`s that always have coinbase
    /// transactions.
    ///
    /// Also applies `network_upgrade_override`, if present.
    pub fn coinbase_strategy(
        network_upgrade_override: impl Into<Option<NetworkUpgrade>>,
        transaction_version_override: impl Into<Option<u32>>,
        transaction_has_valid_network_upgrade: bool,
    ) -> BoxedStrategy<Self> {
        Self::arbitrary_with(LedgerStateOverride {
            height_override: None,
            previous_block_hash_override: None,
            network_upgrade_override: network_upgrade_override.into(),
            transaction_version_override: transaction_version_override.into(),
            transaction_has_valid_network_upgrade,
            always_has_coinbase: true,
        })
    }

    /// Returns a strategy for creating `LedgerState`s that start with a genesis
    /// block.
    ///
    /// These strategies also have coinbase transactions, and an optional network
    /// upgrade override.
    ///
    /// Use the `Genesis` network upgrade to get a random genesis block, with
    /// Zcash genesis features.
    pub fn genesis_strategy(
        network_upgrade_override: impl Into<Option<NetworkUpgrade>>,
        transaction_version_override: impl Into<Option<u32>>,
        transaction_has_valid_network_upgrade: bool,
    ) -> BoxedStrategy<Self> {
        Self::arbitrary_with(LedgerStateOverride {
            height_override: Some(Height(0)),
            previous_block_hash_override: Some(GENESIS_PREVIOUS_BLOCK_HASH),
            network_upgrade_override: network_upgrade_override.into(),
            transaction_version_override: transaction_version_override.into(),
            transaction_has_valid_network_upgrade,
            always_has_coinbase: true,
        })
    }

    /// Returns a strategy for creating `LedgerState`s that start at `height`.
    ///
    /// These strategies also have coinbase transactions, and an optional network
    /// upgrade override.
    pub fn height_strategy(
        height: Height,
        network_upgrade_override: impl Into<Option<NetworkUpgrade>>,
        transaction_version_override: impl Into<Option<u32>>,
        transaction_has_valid_network_upgrade: bool,
    ) -> BoxedStrategy<Self> {
        Self::arbitrary_with(LedgerStateOverride {
            height_override: Some(height),
            previous_block_hash_override: None,
            network_upgrade_override: network_upgrade_override.into(),
            transaction_version_override: transaction_version_override.into(),
            transaction_has_valid_network_upgrade,
            always_has_coinbase: true,
        })
    }

    /// Returns the network upgrade for this ledger state.
    ///
    /// If `network_upgrade_override` is set, it replaces the upgrade calculated
    /// using `height` and `network`.
    pub fn network_upgrade(&self) -> NetworkUpgrade {
        if let Some(network_upgrade_override) = self.network_upgrade_override {
            network_upgrade_override
        } else {
            NetworkUpgrade::current(self.network, self.height)
        }
    }

    /// Returns the transaction version override.
    pub fn transaction_version_override(&self) -> Option<u32> {
        self.transaction_version_override
    }

    /// Returns `true` if all transactions have valid network upgrade fields.
    ///
    /// If `false`, some transactions have invalid network upgrades.
    pub fn transaction_has_valid_network_upgrade(&self) -> bool {
        self.transaction_has_valid_network_upgrade
    }
}

impl Default for LedgerState {
    fn default() -> Self {
        // TODO: stop having a default network
        let default_network = Network::default();
        let default_override = LedgerStateOverride::default();

        let most_recent_nu = NetworkUpgrade::current(default_network, Height::MAX);
        let most_recent_activation_height =
            most_recent_nu.activation_height(default_network).unwrap();

        LedgerState {
            height: most_recent_activation_height,
            network: default_network,
            network_upgrade_override: default_override.network_upgrade_override,
            previous_block_hash_override: default_override.previous_block_hash_override,
            transaction_version_override: default_override.transaction_version_override,
            transaction_has_valid_network_upgrade: default_override
                .transaction_has_valid_network_upgrade,
            has_coinbase: default_override.always_has_coinbase,
        }
    }
}

impl Default for LedgerStateOverride {
    fn default() -> Self {
        let default_network = Network::default();

        // TODO: dynamically select any future network upgrade (#1974)
        let nu5_activation_height = Nu5.activation_height(default_network);
        let nu5_override = if nu5_activation_height.is_some() {
            None
        } else {
            Some(Nu5)
        };

        LedgerStateOverride {
            height_override: None,
            previous_block_hash_override: None,
            network_upgrade_override: nu5_override,
            transaction_version_override: None,
            transaction_has_valid_network_upgrade: false,
            always_has_coinbase: true,
        }
    }
}

impl Arbitrary for LedgerState {
    type Parameters = LedgerStateOverride;

    /// Generate an arbitrary [`LedgerState`].
    ///
    /// The default strategy arbitrarily skips some coinbase transactions, and
    /// has an arbitrary start height. To override, use a specific [`LegderState`]
    /// strategy method.
    fn arbitrary_with(ledger_override: Self::Parameters) -> Self::Strategy {
        (
            any::<Height>(),
            any::<Network>(),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(
                move |(height, network, transaction_has_valid_network_upgrade, has_coinbase)| {
                    LedgerState {
                        height: ledger_override.height_override.unwrap_or(height),
                        network,
                        network_upgrade_override: ledger_override.network_upgrade_override,
                        previous_block_hash_override: ledger_override.previous_block_hash_override,
                        transaction_version_override: ledger_override.transaction_version_override,
                        transaction_has_valid_network_upgrade: ledger_override
                            .transaction_has_valid_network_upgrade
                            || transaction_has_valid_network_upgrade,
                        has_coinbase: ledger_override.always_has_coinbase || has_coinbase,
                    }
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Block {
    type Parameters = LedgerState;

    fn arbitrary_with(ledger_state: Self::Parameters) -> Self::Strategy {
        let transactions_strategy = Transaction::vec_strategy(ledger_state, 2);

        (Header::arbitrary_with(ledger_state), transactions_strategy)
            .prop_map(move |(header, transactions)| Self {
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
        mut current: LedgerState,
        count: usize,
    ) -> BoxedStrategy<SummaryDebug<Vec<Arc<Self>>>> {
        let mut vec = Vec::with_capacity(count);

        // generate block strategies with the correct heights
        for _ in 0..count {
            vec.push(Block::arbitrary_with(current));
            current.height.0 += 1;
        }

        // after the vec strategy generates blocks, fixup invalid parts of the blocks
        vec.prop_map(|mut vec| {
            let mut previous_block_hash = None;
            let mut utxos = HashSet::<transparent::OutPoint>::new();

            for block in vec.iter_mut() {
                // fixup the previous block hash
                if let Some(previous_block_hash) = previous_block_hash {
                    block.header.previous_block_hash = previous_block_hash;
                }
                previous_block_hash = Some(block.hash());

                // fixup the transparent spends
                let mut new_transactions = Vec::new();
                for transaction in block.transactions.drain(..) {
                    let mut transaction = (*transaction).clone();
                    let mut new_inputs = Vec::new();

                    for mut input in transaction.inputs_mut().drain(..) {
                        if let PrevOut {
                            ref mut outpoint, ..
                        } = input
                        {
                            // take a UTXO if available
                            if utxos.remove(outpoint) {
                                new_inputs.push(input);
                            } else if let Some(arbitrary_utxo) = utxos.clone().iter().next() {
                                *outpoint = *arbitrary_utxo;
                                utxos.remove(arbitrary_utxo);
                                new_inputs.push(input);
                            }
                            // otherwise, drop the invalid input, it has no UTXOs to spend
                        } else {
                            // preserve coinbase inputs
                            new_inputs.push(input);
                        }
                    }

                    // delete invalid inputs
                    *transaction.inputs_mut() = new_inputs;

                    // keep transactions with valid input counts
                    // coinbase transactions will never fail this check
                    // this is the input check from `has_inputs_and_outputs`
                    if !transaction.inputs().is_empty()
                        || transaction.joinsplit_count() > 0
                        || transaction.sapling_spends_per_anchor().count() > 0
                        || (transaction.orchard_actions().count() > 0
                            && transaction
                                .orchard_flags()
                                .unwrap_or_else(orchard::Flags::empty)
                                .contains(orchard::Flags::ENABLE_SPENDS))
                    {
                        // skip genesis created UTXOs, just like the state does
                        if block.header.previous_block_hash != GENESIS_PREVIOUS_BLOCK_HASH {
                            // add the created UTXOs
                            // these outputs can be spent from the next transaction in this block onwards
                            // see `new_outputs` for details
                            let hash = transaction.hash();
                            for output_index_in_transaction in 0..transaction.outputs().len() {
                                utxos.insert(transparent::OutPoint {
                                    hash,
                                    index: output_index_in_transaction.try_into().unwrap(),
                                });
                            }
                        }

                        // and keep the transaction
                        new_transactions.push(Arc::new(transaction));
                    }
                }

                // delete invalid transactions
                block.transactions = new_transactions;

                // TODO: fixup the history and authorizing data commitments, if needed
            }
            SummaryDebug(vec.into_iter().map(Arc::new).collect())
        })
        .boxed()
    }
}

impl Arbitrary for Commitment {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (any::<[u8; 32]>(), any::<Network>(), any::<Height>())
            .prop_map(|(commitment_bytes, network, block_height)| {
                if block_height == Heartwood.activation_height(network).unwrap() {
                    Commitment::ChainHistoryActivationReserved
                } else {
                    Commitment::from_bytes(commitment_bytes, network, block_height)
                        .expect("unexpected failure in from_bytes parsing")
                }
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Header {
    type Parameters = LedgerState;

    fn arbitrary_with(ledger_state: Self::Parameters) -> Self::Strategy {
        (
            // version is interpreted as i32 in the spec, so we are limited to i32::MAX here
            (4u32..(i32::MAX as u32)),
            any::<Hash>(),
            any::<merkle::Root>(),
            any::<[u8; 32]>(),
            serialization::arbitrary::datetime_u32(),
            any::<CompactDifficulty>(),
            any::<[u8; 32]>(),
            any::<equihash::Solution>(),
        )
            .prop_map(
                move |(
                    version,
                    mut previous_block_hash,
                    merkle_root,
                    commitment_bytes,
                    time,
                    difficulty_threshold,
                    nonce,
                    solution,
                )| {
                    if let Some(previous_block_hash_override) =
                        ledger_state.previous_block_hash_override
                    {
                        previous_block_hash = previous_block_hash_override;
                    } else if ledger_state.height == Height(0) {
                        previous_block_hash = GENESIS_PREVIOUS_BLOCK_HASH;
                    }

                    Header {
                        version,
                        previous_block_hash,
                        merkle_root,
                        commitment_bytes,
                        time,
                        difficulty_threshold,
                        nonce,
                        solution,
                    }
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
