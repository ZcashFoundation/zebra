use std::{convert::TryInto, sync::Arc};

use chrono::{TimeZone, Utc};
use proptest::{arbitrary::any, array, collection::vec, option, prelude::*};

use crate::{
    amount::Amount,
    block, orchard,
    parameters::NetworkUpgrade,
    primitives::{
        redpallas::{Binding, Signature},
        Bctv14Proof, Groth16Proof, Halo2Proof, ZkSnarkProof,
    },
    sapling, sprout, transparent, LedgerState,
};

use itertools::Itertools;

use super::{FieldNotPresent, JoinSplitData, LockTime, Memo, Transaction};
use sapling::{AnchorVariant, PerSpendAnchor, SharedAnchor};

impl Transaction {
    /// Generate a proptest strategy for V1 Transactions
    pub fn v1_strategy(ledger_state: LedgerState) -> BoxedStrategy<Self> {
        (
            transparent::Input::vec_strategy(ledger_state, 10),
            vec(any::<transparent::Output>(), 0..10),
            any::<LockTime>(),
        )
            .prop_map(|(inputs, outputs, lock_time)| Transaction::V1 {
                inputs,
                outputs,
                lock_time,
            })
            .boxed()
    }

    /// Generate a proptest strategy for V2 Transactions
    pub fn v2_strategy(ledger_state: LedgerState) -> BoxedStrategy<Self> {
        (
            transparent::Input::vec_strategy(ledger_state, 10),
            vec(any::<transparent::Output>(), 0..10),
            any::<LockTime>(),
            option::of(any::<JoinSplitData<Bctv14Proof>>()),
        )
            .prop_map(
                |(inputs, outputs, lock_time, joinsplit_data)| Transaction::V2 {
                    inputs,
                    outputs,
                    lock_time,
                    joinsplit_data,
                },
            )
            .boxed()
    }

    /// Generate a proptest strategy for V3 Transactions
    pub fn v3_strategy(ledger_state: LedgerState) -> BoxedStrategy<Self> {
        (
            transparent::Input::vec_strategy(ledger_state, 10),
            vec(any::<transparent::Output>(), 0..10),
            any::<LockTime>(),
            any::<block::Height>(),
            option::of(any::<JoinSplitData<Bctv14Proof>>()),
        )
            .prop_map(
                |(inputs, outputs, lock_time, expiry_height, joinsplit_data)| Transaction::V3 {
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    joinsplit_data,
                },
            )
            .boxed()
    }

    /// Generate a proptest strategy for V4 Transactions
    pub fn v4_strategy(ledger_state: LedgerState) -> BoxedStrategy<Self> {
        (
            transparent::Input::vec_strategy(ledger_state, 10),
            vec(any::<transparent::Output>(), 0..10),
            any::<LockTime>(),
            any::<block::Height>(),
            option::of(any::<JoinSplitData<Groth16Proof>>()),
            option::of(any::<sapling::ShieldedData<sapling::PerSpendAnchor>>()),
        )
            .prop_map(
                |(
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    joinsplit_data,
                    sapling_shielded_data,
                )| Transaction::V4 {
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    joinsplit_data,
                    sapling_shielded_data,
                },
            )
            .boxed()
    }

    /// Generate a proptest strategy for V5 Transactions
    pub fn v5_strategy(ledger_state: LedgerState) -> BoxedStrategy<Self> {
        (
            Self::branch_id_strategy(),
            any::<LockTime>(),
            any::<block::Height>(),
            transparent::Input::vec_strategy(ledger_state, 10),
            vec(any::<transparent::Output>(), 0..10),
            option::of(any::<sapling::ShieldedData<sapling::SharedAnchor>>()),
            // TODO: uncomment this after the serialization and deserialization is ready
            //option::of(any::<orchard::ShieldedData>()),
        )
            .prop_map(
                |(
                    network_upgrade,
                    lock_time,
                    expiry_height,
                    inputs,
                    outputs,
                    sapling_shielded_data,
                    // TODO: uncomment this after the serialization and deserialization is ready
                    //orchard_shielded_data,
                )| {
                    Transaction::V5 {
                        network_upgrade,
                        lock_time,
                        expiry_height,
                        inputs,
                        outputs,
                        sapling_shielded_data,
                        // TODO: remove the None after the serialization and deserialization is ready
                        orchard_shielded_data: None,
                    }
                },
            )
            .boxed()
    }

    // A custom strategy to use only some of the NetworkUpgrade values
    fn branch_id_strategy() -> BoxedStrategy<NetworkUpgrade> {
        prop_oneof![
            Just(NetworkUpgrade::Overwinter),
            Just(NetworkUpgrade::Sapling),
            Just(NetworkUpgrade::Blossom),
            Just(NetworkUpgrade::Heartwood),
            Just(NetworkUpgrade::Canopy),
            Just(NetworkUpgrade::Nu5),
            // TODO: add future network upgrades
        ]
        .boxed()
    }

    /// Proptest Strategy for creating a Vector of transactions where the first
    /// transaction is always the only coinbase transaction
    pub fn vec_strategy(
        mut ledger_state: LedgerState,
        len: usize,
    ) -> BoxedStrategy<Vec<Arc<Self>>> {
        let coinbase = Transaction::arbitrary_with(ledger_state).prop_map(Arc::new);
        ledger_state.has_coinbase = false;
        let remainder = vec(
            Transaction::arbitrary_with(ledger_state).prop_map(Arc::new),
            len,
        );

        (coinbase, remainder)
            .prop_map(|(first, mut remainder)| {
                remainder.insert(0, first);
                remainder
            })
            .boxed()
    }
}

impl Arbitrary for Memo {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 512))
            .prop_map(|v| {
                let mut bytes = [0; 512];
                bytes.copy_from_slice(v.as_slice());
                Memo(Box::new(bytes))
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for LockTime {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        prop_oneof![
            (block::Height::MIN.0..=block::Height::MAX.0)
                .prop_map(|n| LockTime::Height(block::Height(n))),
            (LockTime::MIN_TIMESTAMP..=LockTime::MAX_TIMESTAMP)
                .prop_map(|n| { LockTime::Time(Utc.timestamp(n as i64, 0)) })
        ]
        .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl<P: ZkSnarkProof + Arbitrary + 'static> Arbitrary for JoinSplitData<P> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<sprout::JoinSplit<P>>(),
            vec(any::<sprout::JoinSplit<P>>(), 0..10),
            array::uniform32(any::<u8>()),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(first, rest, pub_key_bytes, sig_bytes)| Self {
                first,
                rest,
                pub_key: ed25519_zebra::VerificationKeyBytes::from(pub_key_bytes),
                sig: ed25519_zebra::Signature::from({
                    let mut b = [0u8; 64];
                    b.copy_from_slice(sig_bytes.as_slice());
                    b
                }),
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl<AnchorV> Arbitrary for sapling::ShieldedData<AnchorV>
where
    AnchorV: AnchorVariant + Clone + std::fmt::Debug + 'static,
    sapling::TransferData<AnchorV>: Arbitrary,
{
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Amount>(),
            any::<sapling::TransferData<AnchorV>>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(value_balance, transfers, sig_bytes)| Self {
                value_balance,
                transfers,
                binding_sig: redjubjub::Signature::from({
                    let mut b = [0u8; 64];
                    b.copy_from_slice(sig_bytes.as_slice());
                    b
                }),
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for sapling::TransferData<PerSpendAnchor> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        // TODO: add an extra spend or output using Either, and stop using filter_map
        (
            vec(any::<sapling::Spend<PerSpendAnchor>>(), 0..10),
            vec(any::<sapling::Output>(), 0..10),
        )
            .prop_filter_map(
                "arbitrary v4 transfers with no spends and no outputs",
                |(spends, outputs)| {
                    if !spends.is_empty() {
                        Some(sapling::TransferData::SpendsAndMaybeOutputs {
                            shared_anchor: FieldNotPresent,
                            spends: spends.try_into().unwrap(),
                            maybe_outputs: outputs,
                        })
                    } else if !outputs.is_empty() {
                        Some(sapling::TransferData::JustOutputs {
                            outputs: outputs.try_into().unwrap(),
                        })
                    } else {
                        None
                    }
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for sapling::TransferData<SharedAnchor> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        // TODO: add an extra spend or output using Either, and stop using filter_map
        (
            any::<sapling::tree::Root>(),
            vec(any::<sapling::Spend<SharedAnchor>>(), 0..10),
            vec(any::<sapling::Output>(), 0..10),
        )
            .prop_filter_map(
                "arbitrary v5 transfers with no spends and no outputs",
                |(shared_anchor, spends, outputs)| {
                    if !spends.is_empty() {
                        Some(sapling::TransferData::SpendsAndMaybeOutputs {
                            shared_anchor,
                            spends: spends.try_into().unwrap(),
                            maybe_outputs: outputs,
                        })
                    } else if !outputs.is_empty() {
                        Some(sapling::TransferData::JustOutputs {
                            outputs: outputs.try_into().unwrap(),
                        })
                    } else {
                        None
                    }
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for orchard::ShieldedData {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<orchard::shielded_data::Flags>(),
            any::<Amount>(),
            any::<orchard::tree::Root>(),
            any::<Halo2Proof>(),
            vec(any::<orchard::shielded_data::AuthorizedAction>(), 1..10),
            vec(any::<u8>(), 64),
        )
            .prop_map(
                |(flags, value_balance, shared_anchor, proof, actions, sig_bytes)| Self {
                    flags,
                    value_balance,
                    shared_anchor,
                    proof,
                    actions: actions.try_into().expect("we always should have something"),
                    binding_sig: Signature::<Binding>::from({
                        let mut b = [0u8; 64];
                        b.copy_from_slice(sig_bytes.as_slice());
                        b
                    }),
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Transaction {
    type Parameters = LedgerState;

    fn arbitrary_with(ledger_state: Self::Parameters) -> Self::Strategy {
        match ledger_state.network_upgrade() {
            NetworkUpgrade::Genesis | NetworkUpgrade::BeforeOverwinter => {
                Self::v1_strategy(ledger_state)
            }
            NetworkUpgrade::Overwinter => Self::v2_strategy(ledger_state),
            NetworkUpgrade::Sapling => Self::v3_strategy(ledger_state),
            NetworkUpgrade::Blossom | NetworkUpgrade::Heartwood | NetworkUpgrade::Canopy => {
                Self::v4_strategy(ledger_state)
            }
            NetworkUpgrade::Nu5 => prop_oneof![
                Self::v4_strategy(ledger_state),
                Self::v5_strategy(ledger_state)
            ]
            .boxed(),
        }
    }

    type Strategy = BoxedStrategy<Self>;
}

// Utility functions

/// The network upgrade for any fake transactions we will create.
const FAKE_NETWORK_UPGRADE: NetworkUpgrade = NetworkUpgrade::Nu5;

/// Convert `trans` into a fake v5 transaction,
/// converting sapling shielded data from v4 to v5 if possible.
pub fn transaction_to_fake_v5(trans: &Transaction) -> Transaction {
    use Transaction::*;

    match trans {
        V1 {
            inputs,
            outputs,
            lock_time,
        } => V5 {
            network_upgrade: FAKE_NETWORK_UPGRADE,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: block::Height(0),
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        },
        V2 {
            inputs,
            outputs,
            lock_time,
            ..
        } => V5 {
            network_upgrade: FAKE_NETWORK_UPGRADE,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: block::Height(0),
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        },
        V3 {
            inputs,
            outputs,
            lock_time,
            expiry_height,
            ..
        } => V5 {
            network_upgrade: FAKE_NETWORK_UPGRADE,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: *expiry_height,
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        },
        V4 {
            inputs,
            outputs,
            lock_time,
            expiry_height,
            sapling_shielded_data,
            ..
        } => V5 {
            network_upgrade: FAKE_NETWORK_UPGRADE,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: *expiry_height,
            sapling_shielded_data: sapling_shielded_data
                .clone()
                .map(sapling_shielded_v4_to_fake_v5)
                .flatten(),
            orchard_shielded_data: None,
        },
        v5 @ V5 { .. } => v5.clone(),
    }
}

/// Convert a v4 sapling shielded data into a fake v5 sapling shielded data,
/// if possible.
fn sapling_shielded_v4_to_fake_v5(
    v4_shielded: sapling::ShieldedData<PerSpendAnchor>,
) -> Option<sapling::ShieldedData<SharedAnchor>> {
    use sapling::ShieldedData;
    use sapling::TransferData::*;

    let unique_anchors: Vec<_> = v4_shielded
        .spends()
        .map(|spend| spend.per_spend_anchor)
        .unique()
        .collect();

    let fake_spends: Vec<_> = v4_shielded
        .spends()
        .cloned()
        .map(sapling_spend_v4_to_fake_v5)
        .collect();

    let transfers = match v4_shielded.transfers {
        SpendsAndMaybeOutputs { maybe_outputs, .. } => {
            let shared_anchor = match unique_anchors.as_slice() {
                [unique_anchor] => *unique_anchor,
                // Multiple different anchors, can't convert to v5
                _ => return None,
            };

            SpendsAndMaybeOutputs {
                shared_anchor,
                spends: fake_spends.try_into().unwrap(),
                maybe_outputs,
            }
        }
        JustOutputs { outputs } => JustOutputs { outputs },
    };

    let fake_shielded_v5 = ShieldedData::<SharedAnchor> {
        value_balance: v4_shielded.value_balance,
        transfers,
        binding_sig: v4_shielded.binding_sig,
    };

    Some(fake_shielded_v5)
}

/// Convert a v4 sapling spend into a fake v5 sapling spend.
fn sapling_spend_v4_to_fake_v5(
    v4_spend: sapling::Spend<PerSpendAnchor>,
) -> sapling::Spend<SharedAnchor> {
    use sapling::Spend;

    Spend::<SharedAnchor> {
        cv: v4_spend.cv,
        per_spend_anchor: FieldNotPresent,
        nullifier: v4_spend.nullifier,
        rk: v4_spend.rk,
        zkproof: v4_spend.zkproof,
        spend_auth_sig: v4_spend.spend_auth_sig,
    }
}
