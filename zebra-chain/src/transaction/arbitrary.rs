//! Arbitrary data generation for transaction proptests

use std::{cmp::max, collections::HashMap, ops::Neg, sync::Arc};

use chrono::{TimeZone, Utc};
use proptest::{array, collection::vec, option, prelude::*, test_runner::TestRunner};
use reddsa::{orchard::Binding, Signature};

use crate::{
    amount::{self, Amount, NegativeAllowed, NonNegative},
    at_least_one,
    block::{self, arbitrary::MAX_PARTIAL_CHAIN_BLOCKS},
    orchard,
    parameters::{Network, NetworkUpgrade},
    primitives::{Bctv14Proof, Groth16Proof, Halo2Proof, ZkSnarkProof},
    sapling::{self, AnchorVariant, PerSpendAnchor, SharedAnchor},
    serialization::{self, ZcashDeserializeInto},
    sprout, transparent,
    value_balance::{ValueBalance, ValueBalanceError},
    LedgerState,
};

use itertools::Itertools;

use super::{
    FieldNotPresent, JoinSplitData, LockTime, Memo, Transaction, UnminedTx, VerifiedUnminedTx,
};

/// The maximum number of arbitrary transactions, inputs, or outputs.
///
/// This size is chosen to provide interesting behaviour, but not be too large
/// for debugging.
pub const MAX_ARBITRARY_ITEMS: usize = 4;

// TODO: if needed, fixup transaction outputs
//       (currently 0..=9 outputs, consensus rules require 1..)
impl Transaction {
    /// Generate a proptest strategy for V1 Transactions
    pub fn v1_strategy(ledger_state: LedgerState) -> BoxedStrategy<Self> {
        (
            transparent::Input::vec_strategy(&ledger_state, MAX_ARBITRARY_ITEMS),
            vec(any::<transparent::Output>(), 0..MAX_ARBITRARY_ITEMS),
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
            transparent::Input::vec_strategy(&ledger_state, MAX_ARBITRARY_ITEMS),
            vec(any::<transparent::Output>(), 0..MAX_ARBITRARY_ITEMS),
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
            transparent::Input::vec_strategy(&ledger_state, MAX_ARBITRARY_ITEMS),
            vec(any::<transparent::Output>(), 0..MAX_ARBITRARY_ITEMS),
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
            transparent::Input::vec_strategy(&ledger_state, MAX_ARBITRARY_ITEMS),
            vec(any::<transparent::Output>(), 0..MAX_ARBITRARY_ITEMS),
            any::<LockTime>(),
            any::<block::Height>(),
            option::of(any::<JoinSplitData<Groth16Proof>>()),
            option::of(any::<sapling::ShieldedData<sapling::PerSpendAnchor>>()),
        )
            .prop_map(
                move |(
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    joinsplit_data,
                    sapling_shielded_data,
                )| {
                    Transaction::V4 {
                        inputs,
                        outputs,
                        lock_time,
                        expiry_height,
                        joinsplit_data: if ledger_state.height.is_min() {
                            // The genesis block should not contain any joinsplits.
                            None
                        } else {
                            joinsplit_data
                        },
                        sapling_shielded_data: if ledger_state.height.is_min() {
                            // The genesis block should not contain any shielded data.
                            None
                        } else {
                            sapling_shielded_data
                        },
                    }
                },
            )
            .boxed()
    }

    /// Generate a proptest strategy for V5 Transactions
    pub fn v5_strategy(ledger_state: LedgerState) -> BoxedStrategy<Self> {
        (
            NetworkUpgrade::branch_id_strategy(),
            any::<LockTime>(),
            any::<block::Height>(),
            transparent::Input::vec_strategy(&ledger_state, MAX_ARBITRARY_ITEMS),
            vec(any::<transparent::Output>(), 0..MAX_ARBITRARY_ITEMS),
            option::of(any::<sapling::ShieldedData<sapling::SharedAnchor>>()),
            option::of(any::<orchard::ShieldedData>()),
        )
            .prop_map(
                move |(
                    network_upgrade,
                    lock_time,
                    expiry_height,
                    inputs,
                    outputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                )| {
                    Transaction::V5 {
                        network_upgrade: if ledger_state.transaction_has_valid_network_upgrade() {
                            ledger_state.network_upgrade()
                        } else {
                            network_upgrade
                        },
                        lock_time,
                        expiry_height,
                        inputs,
                        outputs,
                        sapling_shielded_data: if ledger_state.height.is_min() {
                            // The genesis block should not contain any shielded data.
                            None
                        } else {
                            sapling_shielded_data
                        },
                        orchard_shielded_data: if ledger_state.height.is_min() {
                            // The genesis block should not contain any shielded data.
                            None
                        } else {
                            orchard_shielded_data
                        },
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
        // TODO: fixup coinbase miner subsidy
        let coinbase = Transaction::arbitrary_with(ledger_state.clone()).prop_map(Arc::new);
        ledger_state.has_coinbase = false;
        let remainder = vec(
            Transaction::arbitrary_with(ledger_state).prop_map(Arc::new),
            0..=len,
        );

        (coinbase, remainder)
            .prop_map(|(first, mut remainder)| {
                remainder.insert(0, first);
                remainder
            })
            .boxed()
    }

    /// Apply `f` to the transparent output, `v_sprout_new`, and `v_sprout_old` values
    /// in this transaction, regardless of version.
    pub fn for_each_value_mut<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut Amount<NonNegative>),
    {
        for output_value in self.output_values_mut() {
            f(output_value);
        }

        for sprout_added_value in self.output_values_to_sprout_mut() {
            f(sprout_added_value);
        }
        for sprout_removed_value in self.input_values_from_sprout_mut() {
            f(sprout_removed_value);
        }
    }

    /// Apply `f` to the sapling value balance and orchard value balance
    /// in this transaction, regardless of version.
    pub fn for_each_value_balance_mut<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut Amount<NegativeAllowed>),
    {
        if let Some(sapling_value_balance) = self.sapling_value_balance_mut() {
            f(sapling_value_balance);
        }

        if let Some(orchard_value_balance) = self.orchard_value_balance_mut() {
            f(orchard_value_balance);
        }
    }

    /// Fixup transparent values and shielded value balances,
    /// so that transaction and chain value pools won't overflow MAX_MONEY.
    ///
    /// These fixes are applied to coinbase and non-coinbase transactions.
    //
    // TODO: do we want to allow overflow, based on an arbitrary bool?
    pub fn fix_overflow(&mut self) {
        fn scale_to_avoid_overflow<C: amount::Constraint>(amount: &mut Amount<C>)
        where
            Amount<C>: Copy,
        {
            const POOL_COUNT: u64 = 4;

            let max_arbitrary_items: u64 = MAX_ARBITRARY_ITEMS.try_into().unwrap();
            let max_partial_chain_blocks: u64 = MAX_PARTIAL_CHAIN_BLOCKS.try_into().unwrap();

            // inputs/joinsplits/spends|outputs/actions * pools * transactions
            let transaction_pool_scaling_divisor =
                max_arbitrary_items * POOL_COUNT * max_arbitrary_items;
            // inputs/joinsplits/spends|outputs/actions * transactions * blocks
            let chain_pool_scaling_divisor =
                max_arbitrary_items * max_arbitrary_items * max_partial_chain_blocks;
            let scaling_divisor = max(transaction_pool_scaling_divisor, chain_pool_scaling_divisor);

            *amount = (*amount / scaling_divisor).expect("divisor is not zero");
        }

        self.for_each_value_mut(scale_to_avoid_overflow);
        self.for_each_value_balance_mut(scale_to_avoid_overflow);
    }

    /// Fixup transparent values and shielded value balances,
    /// so that this transaction passes the "non-negative chain value pool" checks.
    /// (These checks use the sum of unspent outputs for each transparent and shielded pool.)
    ///
    /// These fixes are applied to coinbase and non-coinbase transactions.
    ///
    /// `chain_value_pools` contains the chain value pool balances,
    /// as of the previous transaction in this block
    /// (or the last transaction in the previous block).
    ///
    /// `outputs` must contain all the [`transparent::Output`]s spent in this transaction.
    ///
    /// Currently, these fixes almost always leave some remaining value in each transparent
    /// and shielded chain value pool.
    ///
    /// Before fixing the chain value balances, this method calls `fix_overflow`
    /// to make sure that transaction and chain value pools don't overflow MAX_MONEY.
    ///
    /// After fixing the chain value balances, this method calls `fix_remaining_value`
    /// to fix the remaining value in the transaction value pool.
    ///
    /// Returns the remaining transaction value, and the updated chain value balances.
    ///
    /// # Panics
    ///
    /// If any spent [`transparent::Output`] is missing from
    /// [`transparent::OutPoint`]s.
    //
    // TODO: take some extra arbitrary flags, which select between zero and non-zero
    //       remaining value in each chain value pool
    pub fn fix_chain_value_pools(
        &mut self,
        chain_value_pools: ValueBalance<NonNegative>,
        outputs: &HashMap<transparent::OutPoint, transparent::Output>,
    ) -> Result<(Amount<NonNegative>, ValueBalance<NonNegative>), ValueBalanceError> {
        self.fix_overflow();

        // a temporary value used to check that inputs don't break the chain value balance
        // consensus rules
        let mut input_chain_value_pools = chain_value_pools;

        for input in self.inputs() {
            input_chain_value_pools = input_chain_value_pools
                .add_transparent_input(input, outputs)
                .expect("find_valid_utxo_for_spend only spends unspent transparent outputs");
        }

        // update the input chain value pools,
        // zeroing any inputs that would exceed the input value

        // TODO: consensus rule: normalise sprout JoinSplit values
        //       so at least one of the values in each JoinSplit is zero
        for input in self.input_values_from_sprout_mut() {
            match input_chain_value_pools
                .add_chain_value_pool_change(ValueBalance::from_sprout_amount(input.neg()))
            {
                Ok(new_chain_pools) => input_chain_value_pools = new_chain_pools,
                // set the invalid input value to zero
                Err(_) => *input = Amount::zero(),
            }
        }

        // positive value balances subtract from the chain value pool

        let sapling_input = self.sapling_value_balance().constrain::<NonNegative>();
        if let Ok(sapling_input) = sapling_input {
            match input_chain_value_pools.add_chain_value_pool_change(-sapling_input) {
                Ok(new_chain_pools) => input_chain_value_pools = new_chain_pools,
                Err(_) => *self.sapling_value_balance_mut().unwrap() = Amount::zero(),
            }
        }

        let orchard_input = self.orchard_value_balance().constrain::<NonNegative>();
        if let Ok(orchard_input) = orchard_input {
            match input_chain_value_pools.add_chain_value_pool_change(-orchard_input) {
                Ok(new_chain_pools) => input_chain_value_pools = new_chain_pools,
                Err(_) => *self.orchard_value_balance_mut().unwrap() = Amount::zero(),
            }
        }

        let remaining_transaction_value = self.fix_remaining_value(outputs)?;

        // check our calculations are correct
        let transaction_chain_value_pool_change =
            self
            .value_balance_from_outputs(outputs)
            .expect("chain value pool and remaining transaction value fixes produce valid transaction value balances")
            .neg();

        let chain_value_pools = chain_value_pools
            .add_transaction(self, outputs)
            .unwrap_or_else(|err| {
                panic!(
                    "unexpected chain value pool error: {err:?}, \n\
                     original chain value pools: {chain_value_pools:?}, \n\
                     transaction chain value change: {transaction_chain_value_pool_change:?}, \n\
                     input-only transaction chain value pools: {input_chain_value_pools:?}, \n\
                     calculated remaining transaction value: {remaining_transaction_value:?}",
                )
            });

        Ok((remaining_transaction_value, chain_value_pools))
    }

    /// Returns the total input value of this transaction's value pool.
    ///
    /// This is the sum of transparent inputs, sprout input values,
    /// and if positive, the sapling and orchard value balances.
    ///
    /// `outputs` must contain all the [`transparent::Output`]s spent in this transaction.
    fn input_value_pool(
        &self,
        outputs: &HashMap<transparent::OutPoint, transparent::Output>,
    ) -> Result<Amount<NonNegative>, ValueBalanceError> {
        let transparent_inputs = self
            .inputs()
            .iter()
            .map(|input| input.value_from_outputs(outputs))
            .sum::<Result<Amount<NonNegative>, amount::Error>>()
            .map_err(ValueBalanceError::Transparent)?;
        // TODO: fix callers which cause overflows, check for:
        //       cached `outputs` that don't go through `fix_overflow`, and
        //       values much larger than MAX_MONEY
        //.expect("chain is limited to MAX_MONEY");

        let sprout_inputs = self
            .input_values_from_sprout()
            .sum::<Result<Amount<NonNegative>, amount::Error>>()
            .expect("chain is limited to MAX_MONEY");

        // positive value balances add to the transaction value pool
        let sapling_input = self
            .sapling_value_balance()
            .sapling_amount()
            .constrain::<NonNegative>()
            .unwrap_or_else(|_| Amount::zero());

        let orchard_input = self
            .orchard_value_balance()
            .orchard_amount()
            .constrain::<NonNegative>()
            .unwrap_or_else(|_| Amount::zero());

        let transaction_input_value_pool =
            (transparent_inputs + sprout_inputs + sapling_input + orchard_input)
                .expect("chain is limited to MAX_MONEY");

        Ok(transaction_input_value_pool)
    }

    /// Fixup non-coinbase transparent values and shielded value balances,
    /// so that this transaction passes the "non-negative remaining transaction value"
    /// check. (This check uses the sum of inputs minus outputs.)
    ///
    /// Returns the remaining transaction value.
    ///
    /// `outputs` must contain all the [`transparent::Output`]s spent in this transaction.
    ///
    /// Currently, these fixes almost always leave some remaining value in the
    /// transaction value pool.
    ///
    /// # Panics
    ///
    /// If any spent [`transparent::Output`] is missing from
    /// [`transparent::OutPoint`]s.
    //
    // TODO: split this method up, after we've implemented chain value balance adjustments
    //
    // TODO: take an extra arbitrary bool, which selects between zero and non-zero
    //       remaining value in the transaction value pool
    pub fn fix_remaining_value(
        &mut self,
        outputs: &HashMap<transparent::OutPoint, transparent::Output>,
    ) -> Result<Amount<NonNegative>, ValueBalanceError> {
        if self.is_coinbase() {
            // TODO: if needed, fixup coinbase:
            // - miner subsidy
            // - founders reward or funding streams (hopefully not?)
            // - remaining transaction value

            // Act as if the generated test case spends all the miner subsidy, miner fees, and
            // founders reward / funding stream correctly.
            return Ok(Amount::zero());
        }

        let mut remaining_input_value = self.input_value_pool(outputs)?;

        // assign remaining input value to outputs,
        // zeroing any outputs that would exceed the input value

        for output_value in self.output_values_mut() {
            if remaining_input_value >= *output_value {
                remaining_input_value = (remaining_input_value - *output_value)
                    .expect("input >= output so result is always non-negative");
            } else {
                *output_value = Amount::zero();
            }
        }

        for output_value in self.output_values_to_sprout_mut() {
            if remaining_input_value >= *output_value {
                remaining_input_value = (remaining_input_value - *output_value)
                    .expect("input >= output so result is always non-negative");
            } else {
                *output_value = Amount::zero();
            }
        }

        if let Some(value_balance) = self.sapling_value_balance_mut() {
            if let Ok(output_value) = value_balance.neg().constrain::<NonNegative>() {
                if remaining_input_value >= output_value {
                    remaining_input_value = (remaining_input_value - output_value)
                        .expect("input >= output so result is always non-negative");
                } else {
                    *value_balance = Amount::zero();
                }
            }
        }

        if let Some(value_balance) = self.orchard_value_balance_mut() {
            if let Ok(output_value) = value_balance.neg().constrain::<NonNegative>() {
                if remaining_input_value >= output_value {
                    remaining_input_value = (remaining_input_value - output_value)
                        .expect("input >= output so result is always non-negative");
                } else {
                    *value_balance = Amount::zero();
                }
            }
        }

        // check our calculations are correct
        let remaining_transaction_value = self
            .value_balance_from_outputs(outputs)
            .expect("chain is limited to MAX_MONEY")
            .remaining_transaction_value()
            .unwrap_or_else(|err| {
                panic!(
                    "unexpected remaining transaction value: {err:?}, \
                     calculated remaining input value: {remaining_input_value:?}"
                )
            });
        assert_eq!(
            remaining_input_value,
            remaining_transaction_value,
            "fix_remaining_value and remaining_transaction_value calculated different remaining values"
        );

        Ok(remaining_transaction_value)
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

/// Generates arbitrary [`LockTime`]s.
impl Arbitrary for LockTime {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        prop_oneof![
            (block::Height::MIN.0..=LockTime::MAX_HEIGHT.0)
                .prop_map(|n| LockTime::Height(block::Height(n))),
            (LockTime::MIN_TIMESTAMP..=LockTime::MAX_TIMESTAMP).prop_map(|n| {
                LockTime::Time(
                    Utc.timestamp_opt(n, 0)
                        .single()
                        .expect("in-range number of seconds and valid nanosecond"),
                )
            })
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
            vec(any::<sprout::JoinSplit<P>>(), 0..MAX_ARBITRARY_ITEMS),
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
        vec(any::<sapling::Output>(), 0..MAX_ARBITRARY_ITEMS)
            .prop_flat_map(|outputs| {
                (
                    if outputs.is_empty() {
                        // must have at least one spend or output
                        vec(
                            any::<sapling::Spend<PerSpendAnchor>>(),
                            1..MAX_ARBITRARY_ITEMS,
                        )
                    } else {
                        vec(
                            any::<sapling::Spend<PerSpendAnchor>>(),
                            0..MAX_ARBITRARY_ITEMS,
                        )
                    },
                    Just(outputs),
                )
            })
            .prop_map(|(spends, outputs)| {
                if !spends.is_empty() {
                    sapling::TransferData::SpendsAndMaybeOutputs {
                        shared_anchor: FieldNotPresent,
                        spends: spends.try_into().unwrap(),
                        maybe_outputs: outputs,
                    }
                } else if !outputs.is_empty() {
                    sapling::TransferData::JustOutputs {
                        outputs: outputs.try_into().unwrap(),
                    }
                } else {
                    unreachable!("there must be at least one generated spend or output")
                }
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for sapling::TransferData<SharedAnchor> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        vec(any::<sapling::Output>(), 0..MAX_ARBITRARY_ITEMS)
            .prop_flat_map(|outputs| {
                (
                    any::<sapling::tree::Root>(),
                    if outputs.is_empty() {
                        // must have at least one spend or output
                        vec(
                            any::<sapling::Spend<SharedAnchor>>(),
                            1..MAX_ARBITRARY_ITEMS,
                        )
                    } else {
                        vec(
                            any::<sapling::Spend<SharedAnchor>>(),
                            0..MAX_ARBITRARY_ITEMS,
                        )
                    },
                    Just(outputs),
                )
            })
            .prop_map(|(shared_anchor, spends, outputs)| {
                if !spends.is_empty() {
                    sapling::TransferData::SpendsAndMaybeOutputs {
                        shared_anchor,
                        spends: spends.try_into().unwrap(),
                        maybe_outputs: outputs,
                    }
                } else if !outputs.is_empty() {
                    sapling::TransferData::JustOutputs {
                        outputs: outputs.try_into().unwrap(),
                    }
                } else {
                    unreachable!("there must be at least one generated spend or output")
                }
            })
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
            vec(
                any::<orchard::shielded_data::AuthorizedAction>(),
                1..MAX_ARBITRARY_ITEMS,
            ),
            any::<BindingSignature>(),
        )
            .prop_map(
                |(flags, value_balance, shared_anchor, proof, actions, binding_sig)| Self {
                    flags,
                    value_balance,
                    shared_anchor,
                    proof,
                    actions: actions
                        .try_into()
                        .expect("arbitrary vector size range produces at least one action"),
                    binding_sig: binding_sig.0,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct BindingSignature(pub(crate) Signature<Binding>);

impl Arbitrary for BindingSignature {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 64))
            .prop_filter_map(
                "zero Signature::<Binding> values are invalid",
                |sig_bytes| {
                    let mut b = [0u8; 64];
                    b.copy_from_slice(sig_bytes.as_slice());
                    if b == [0u8; 64] {
                        return None;
                    }
                    Some(BindingSignature(Signature::<Binding>::from(b)))
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Transaction {
    type Parameters = LedgerState;

    fn arbitrary_with(ledger_state: Self::Parameters) -> Self::Strategy {
        match ledger_state.transaction_version_override() {
            Some(1) => return Self::v1_strategy(ledger_state),
            Some(2) => return Self::v2_strategy(ledger_state),
            Some(3) => return Self::v3_strategy(ledger_state),
            Some(4) => return Self::v4_strategy(ledger_state),
            Some(5) => return Self::v5_strategy(ledger_state),
            Some(_) => unreachable!("invalid transaction version in override"),
            None => {}
        }

        match ledger_state.network_upgrade() {
            NetworkUpgrade::Genesis | NetworkUpgrade::BeforeOverwinter => {
                Self::v1_strategy(ledger_state)
            }
            NetworkUpgrade::Overwinter => Self::v2_strategy(ledger_state),
            NetworkUpgrade::Sapling => Self::v3_strategy(ledger_state),
            NetworkUpgrade::Blossom | NetworkUpgrade::Heartwood | NetworkUpgrade::Canopy => {
                Self::v4_strategy(ledger_state)
            }
            NetworkUpgrade::Nu5
            | NetworkUpgrade::Nu6
            | NetworkUpgrade::Nu6_1
            | NetworkUpgrade::Nu7 => prop_oneof![
                Self::v4_strategy(ledger_state.clone()),
                Self::v5_strategy(ledger_state)
            ]
            .boxed(),
        }
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for UnminedTx {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        any::<Transaction>().prop_map_into().boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for VerifiedUnminedTx {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<UnminedTx>(),
            any::<Amount<NonNegative>>(),
            any::<u64>(),
            any::<(u16, u16)>().prop_map(|(unpaid_actions, conventional_actions)| {
                (
                    unpaid_actions % conventional_actions.saturating_add(1),
                    conventional_actions,
                )
            }),
            any::<f32>(),
            serialization::arbitrary::datetime_u32(),
            any::<block::Height>(),
        )
            .prop_map(
                |(
                    transaction,
                    miner_fee,
                    legacy_sigop_count,
                    (conventional_actions, mut unpaid_actions),
                    fee_weight_ratio,
                    time,
                    height,
                )| {
                    if unpaid_actions > conventional_actions {
                        unpaid_actions = conventional_actions;
                    }

                    let conventional_actions = conventional_actions as u32;
                    let unpaid_actions = unpaid_actions as u32;

                    Self {
                        transaction,
                        miner_fee,
                        legacy_sigop_count,
                        conventional_actions,
                        unpaid_actions,
                        fee_weight_ratio,
                        time: Some(time),
                        height: Some(height),
                    }
                },
            )
            .boxed()
    }
    type Strategy = BoxedStrategy<Self>;
}

// Utility functions

/// Convert `trans` into a fake v5 transaction,
/// converting sapling shielded data from v4 to v5 if possible.
pub fn transaction_to_fake_v5(
    trans: &Transaction,
    network: &Network,
    height: block::Height,
) -> Transaction {
    use Transaction::*;

    let block_nu = NetworkUpgrade::current(network, height);

    match trans {
        V1 {
            inputs,
            outputs,
            lock_time,
        } => V5 {
            network_upgrade: block_nu,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: height,
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        },
        V2 {
            inputs,
            outputs,
            lock_time,
            ..
        } => V5 {
            network_upgrade: block_nu,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: height,
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        },
        V3 {
            inputs,
            outputs,
            lock_time,
            ..
        } => V5 {
            network_upgrade: block_nu,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: height,
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        },
        V4 {
            inputs,
            outputs,
            lock_time,
            sapling_shielded_data,
            ..
        } => V5 {
            network_upgrade: block_nu,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: height,
            sapling_shielded_data: sapling_shielded_data
                .clone()
                .and_then(sapling_shielded_v4_to_fake_v5),
            orchard_shielded_data: None,
        },
        v5 @ V5 { .. } => v5.clone(),
        #[cfg(feature = "tx_v6")]
        v6 @ V6 { .. } => v6.clone(),
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

/// Iterate over V4 transactions in the block test vectors for the specified `network`.
pub fn test_transactions(
    network: &Network,
) -> impl DoubleEndedIterator<Item = (block::Height, Arc<Transaction>)> {
    let blocks = network.block_iter();

    transactions_from_blocks(blocks)
}

/// Returns an iterator over V5 transactions extracted from the given blocks.
pub fn v5_transactions<'b>(
    blocks: impl DoubleEndedIterator<Item = (&'b u32, &'b &'static [u8])> + 'b,
) -> impl DoubleEndedIterator<Item = Transaction> + 'b {
    transactions_from_blocks(blocks).filter_map(|(_, tx)| match *tx {
        Transaction::V1 { .. }
        | Transaction::V2 { .. }
        | Transaction::V3 { .. }
        | Transaction::V4 { .. } => None,
        ref tx @ Transaction::V5 { .. } => Some(tx.clone()),
        #[cfg(feature = "tx_v6")]
        ref tx @ Transaction::V6 { .. } => Some(tx.clone()),
    })
}

/// Generate an iterator over ([`block::Height`], [`Arc<Transaction>`]).
pub fn transactions_from_blocks<'a>(
    blocks: impl DoubleEndedIterator<Item = (&'a u32, &'a &'static [u8])> + 'a,
) -> impl DoubleEndedIterator<Item = (block::Height, Arc<Transaction>)> + 'a {
    blocks.flat_map(|(&block_height, &block_bytes)| {
        let block = block_bytes
            .zcash_deserialize_into::<block::Block>()
            .expect("block is structurally valid");

        block
            .transactions
            .into_iter()
            .map(move |transaction| (block::Height(block_height), transaction))
    })
}

/// Modify a V5 transaction to insert fake Orchard shielded data.
///
/// Creates a fake instance of [`orchard::ShieldedData`] with one fake action. Note that both the
/// action and the shielded data are invalid and shouldn't be used in tests that require them to be
/// valid.
///
/// A mutable reference to the inserted shielded data is returned, so that the caller can further
/// customize it if required.
///
/// # Panics
///
/// Panics if the transaction to be modified is not V5.
pub fn insert_fake_orchard_shielded_data(
    transaction: &mut Transaction,
) -> &mut orchard::ShieldedData {
    // Create a dummy action
    let mut runner = TestRunner::default();
    let dummy_action = orchard::Action::arbitrary()
        .new_tree(&mut runner)
        .unwrap()
        .current();

    // Pair the dummy action with a fake signature
    let dummy_authorized_action = orchard::AuthorizedAction {
        action: dummy_action,
        spend_auth_sig: Signature::from([0u8; 64]),
    };

    // Place the dummy action inside the Orchard shielded data
    let dummy_shielded_data = orchard::ShieldedData {
        flags: orchard::Flags::empty(),
        value_balance: Amount::try_from(0).expect("invalid transaction amount"),
        shared_anchor: orchard::tree::Root::default(),
        proof: Halo2Proof(vec![]),
        actions: at_least_one![dummy_authorized_action],
        binding_sig: Signature::from([0u8; 64]),
    };

    // Replace the shielded data in the transaction
    match transaction {
        Transaction::V5 {
            orchard_shielded_data,
            ..
        } => {
            *orchard_shielded_data = Some(dummy_shielded_data);

            orchard_shielded_data
                .as_mut()
                .expect("shielded data was just inserted")
        }
        _ => panic!("Fake V5 transaction is not V5"),
    }
}
