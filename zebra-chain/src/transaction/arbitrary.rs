use std::sync::Arc;

use block::Height;
use chrono::{TimeZone, Utc};
use futures::future::Either;
use proptest::{arbitrary::any, array, collection::vec, option, prelude::*};

use crate::LedgerState;
use crate::{
    amount::Amount,
    block,
    parameters::NetworkUpgrade,
    primitives::{Bctv14Proof, Groth16Proof, ZkSnarkProof},
    sapling, sprout, transparent,
};

use super::{JoinSplitData, LockTime, Memo, ShieldedData, Transaction};

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
            any::<Amount>(),
            option::of(any::<ShieldedData>()),
            option::of(any::<JoinSplitData<Groth16Proof>>()),
        )
            .prop_map(
                |(
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    value_balance,
                    shielded_data,
                    joinsplit_data,
                )| Transaction::V4 {
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    value_balance,
                    shielded_data,
                    joinsplit_data,
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
        ledger_state.is_coinbase = false;
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

impl Arbitrary for ShieldedData {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            prop_oneof![
                any::<sapling::Spend>().prop_map(Either::Left),
                any::<sapling::Output>().prop_map(Either::Right)
            ],
            vec(any::<sapling::Spend>(), 0..10),
            vec(any::<sapling::Output>(), 0..10),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(first, rest_spends, rest_outputs, sig_bytes)| Self {
                first,
                rest_spends,
                rest_outputs,
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

impl Arbitrary for Transaction {
    type Parameters = LedgerState;

    fn arbitrary_with(ledger_state: Self::Parameters) -> Self::Strategy {
        let LedgerState {
            tip_height,
            network,
            ..
        } = ledger_state;

        let height = Height(tip_height.0 + 1);
        let network_upgrade = NetworkUpgrade::current(network, height);

        match network_upgrade {
            NetworkUpgrade::Genesis | NetworkUpgrade::BeforeOverwinter => {
                Self::v1_strategy(ledger_state)
            }
            NetworkUpgrade::Overwinter => Self::v2_strategy(ledger_state),
            NetworkUpgrade::Sapling => Self::v3_strategy(ledger_state),
            NetworkUpgrade::Blossom | NetworkUpgrade::Heartwood | NetworkUpgrade::Canopy => {
                Self::v4_strategy(ledger_state)
            }
            NetworkUpgrade::NU5 => {
                unimplemented!("NU5 upgrade is still in progress and not fully implemented")
            }
        }
    }

    type Strategy = BoxedStrategy<Self>;
}
