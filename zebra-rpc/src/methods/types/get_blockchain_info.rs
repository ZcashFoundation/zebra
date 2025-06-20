//! Types used in `getblockchaininfo` RPC method.

use derive_getters::Getters;
use derive_new::new;
use zebra_chain::{
    amount::{Amount, NegativeAllowed, NonNegative},
    value_balance::ValueBalance,
};

use zec::Zec;

use super::*;

/// A value pool's balance in Zec and Zatoshis
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
#[serde(rename_all = "camelCase")]
pub struct GetBlockchainInfoBalance {
    /// Name of the pool
    #[serde(skip_serializing_if = "String::is_empty", default)]
    id: String,
    /// Total amount in the pool, in ZEC
    #[getter(copy)]
    chain_value: Zec<NonNegative>,
    /// Total amount in the pool, in zatoshis
    #[getter(copy)]
    chain_value_zat: Amount<NonNegative>,
    /// Whether the value pool balance is being monitored.
    monitored: bool,
    /// Change to the amount in the pool produced by this block, in ZEC
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[getter(copy)]
    value_delta: Option<Zec<NegativeAllowed>>,
    /// Change to the amount in the pool produced by this block, in zatoshis
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[getter(copy)]
    value_delta_zat: Option<Amount>,
}

impl GetBlockchainInfoBalance {
    /// Returns a list of [`GetBlockchainInfoBalance`]s converted from the default [`ValueBalance`].
    pub fn zero_pools() -> [Self; 5] {
        Self::value_pools(Default::default(), None)
    }

    /// Creates a new [`GetBlockchainInfoBalance`] from a pool name and its value balance
    /// and optionally with a delta value.
    pub(crate) fn new_internal(
        id: impl ToString,
        amount: Amount<NonNegative>,
        delta_amount: Option<Amount<NegativeAllowed>>,
    ) -> Self {
        Self {
            id: id.to_string(),
            chain_value: Zec::from(amount),
            chain_value_zat: amount,
            monitored: amount.zatoshis() != 0,
            value_delta: delta_amount.map(Zec::from),
            value_delta_zat: delta_amount,
        }
    }

    /// Creates a [`GetBlockchainInfoBalance`] for the transparent pool.
    pub fn transparent(
        amount: Amount<NonNegative>,
        delta: Option<Amount<NegativeAllowed>>,
    ) -> Self {
        Self::new_internal("transparent", amount, delta)
    }

    /// Creates a [`GetBlockchainInfoBalance`] for the Sprout pool.
    pub fn sprout(amount: Amount<NonNegative>, delta: Option<Amount<NegativeAllowed>>) -> Self {
        Self::new_internal("sprout", amount, delta)
    }

    /// Creates a [`GetBlockchainInfoBalance`] for the Sapling pool.
    pub fn sapling(amount: Amount<NonNegative>, delta: Option<Amount<NegativeAllowed>>) -> Self {
        Self::new_internal("sapling", amount, delta)
    }

    /// Creates a [`GetBlockchainInfoBalance`] for the Orchard pool.
    pub fn orchard(amount: Amount<NonNegative>, delta: Option<Amount<NegativeAllowed>>) -> Self {
        Self::new_internal("orchard", amount, delta)
    }

    /// Creates a [`GetBlockchainInfoBalance`] for the Deferred pool.
    pub fn deferred(amount: Amount<NonNegative>, delta: Option<Amount<NegativeAllowed>>) -> Self {
        Self::new_internal("deferred", amount, delta)
    }

    /// Converts a [`ValueBalance`] to a list of [`GetBlockchainInfoBalance`]s.
    pub fn value_pools(
        value_balance: ValueBalance<NonNegative>,
        delta_balance: Option<ValueBalance<NegativeAllowed>>,
    ) -> [Self; 5] {
        [
            Self::transparent(
                value_balance.transparent_amount(),
                delta_balance.map(|b| b.transparent_amount()),
            ),
            Self::sprout(
                value_balance.sprout_amount(),
                delta_balance.map(|b| b.sprout_amount()),
            ),
            Self::sapling(
                value_balance.sapling_amount(),
                delta_balance.map(|b| b.sapling_amount()),
            ),
            Self::orchard(
                value_balance.orchard_amount(),
                delta_balance.map(|b| b.orchard_amount()),
            ),
            Self::deferred(
                value_balance.deferred_amount(),
                delta_balance.map(|b| b.deferred_amount()),
            ),
        ]
    }

    /// Converts a [`ValueBalance`] to a [`GetBlockchainInfoBalance`] representing the total chain supply.
    pub fn chain_supply(value_balance: ValueBalance<NonNegative>) -> Self {
        Self::value_pools(value_balance, None)
            .into_iter()
            .reduce(|a, b| {
                GetBlockchainInfoBalance::new_internal(
                    "",
                    (a.chain_value_zat + b.chain_value_zat)
                        .expect("sum of value balances should not overflow"),
                    None,
                )
            })
            .expect("at least one pool")
    }
}
