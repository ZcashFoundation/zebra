use std::{convert::TryInto, sync::Arc};

use chrono::{TimeZone, Utc};
use proptest::{arbitrary::any, array, collection::vec, option, prelude::*};

use crate::{
    amount::Amount,
    block,
    parameters::{ConsensusBranchId, NetworkUpgrade},
    primitives::{Bctv14Proof, Groth16Proof, ZkSnarkProof},
    sapling, sprout, transparent, LedgerState,
};

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
            option::of(any::<sapling::ShieldedData<sapling::PerSpendAnchor>>()),
            option::of(any::<JoinSplitData<Groth16Proof>>()),
        )
            .prop_map(
                |(
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    sapling_shielded_data,
                    joinsplit_data,
                )| Transaction::V4 {
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    sapling_shielded_data,
                    joinsplit_data,
                },
            )
            .boxed()
    }

    /// Generate a proptest strategy for V5 Transactions
    pub fn v5_strategy(ledger_state: LedgerState) -> BoxedStrategy<Self> {
        (
            any::<ConsensusBranchId>(),
            any::<LockTime>(),
            any::<block::Height>(),
            transparent::Input::vec_strategy(ledger_state, 10),
            vec(any::<transparent::Output>(), 0..10),
            option::of(any::<sapling::ShieldedData<sapling::SharedAnchor>>()),
        )
            .prop_map(
                |(
                    consensus_branch_id,
                    lock_time,
                    expiry_height,
                    inputs,
                    outputs,
                    sapling_shielded_data,
                )| {
                    Transaction::V5 {
                        consensus_branch_id,
                        lock_time,
                        expiry_height,
                        inputs,
                        outputs,
                        sapling_shielded_data,
                    }
                },
            )
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

impl Arbitrary for ConsensusBranchId {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (any::<u32>()).prop_map(ConsensusBranchId::from).boxed()
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
