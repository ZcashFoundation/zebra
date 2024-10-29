//! Types used in `getblockchaininfo` RPC method.

use zebra_chain::{
    amount::{Amount, NonNegative},
    value_balance::ValueBalance,
};

use super::*;

/// A value pool's balance in Zec and Zatoshis
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValuePoolBalance {
    /// Name of the pool
    id: String,
    /// Total amount in the pool, in ZEC
    chain_value: Zec<NonNegative>,
    /// Total amount in the pool, in zatoshis
    chain_value_zat: Amount<NonNegative>,
}

impl ValuePoolBalance {
    /// Returns a list of [`ValuePoolBalance`]s converted from the default [`ValueBalance`].
    pub fn zero_pools() -> [Self; 5] {
        Self::from_value_balance(Default::default())
    }

    /// Creates a new [`ValuePoolBalance`] from a pool name and its value balance.
    pub fn new(id: impl ToString, amount: Amount<NonNegative>) -> Self {
        Self {
            id: id.to_string(),
            chain_value: Zec::from(amount),
            chain_value_zat: amount,
        }
    }

    /// Creates a [`ValuePoolBalance`] for the transparent pool.
    pub fn transparent(amount: Amount<NonNegative>) -> Self {
        Self::new("transparent", amount)
    }

    /// Creates a [`ValuePoolBalance`] for the Sprout pool.
    pub fn sprout(amount: Amount<NonNegative>) -> Self {
        Self::new("sprout", amount)
    }

    /// Creates a [`ValuePoolBalance`] for the Sapling pool.
    pub fn sapling(amount: Amount<NonNegative>) -> Self {
        Self::new("sapling", amount)
    }

    /// Creates a [`ValuePoolBalance`] for the Orchard pool.
    pub fn orchard(amount: Amount<NonNegative>) -> Self {
        Self::new("orchard", amount)
    }

    /// Creates a [`ValuePoolBalance`] for the Deferred pool.
    pub fn deferred(amount: Amount<NonNegative>) -> Self {
        Self::new("deferred", amount)
    }

    /// Converts a [`ValueBalance`] to a list of [`ValuePoolBalance`]s.
    pub fn from_value_balance(value_balance: ValueBalance<NonNegative>) -> [Self; 5] {
        [
            Self::transparent(value_balance.transparent_amount()),
            Self::sprout(value_balance.sprout_amount()),
            Self::sapling(value_balance.sapling_amount()),
            Self::orchard(value_balance.orchard_amount()),
            Self::deferred(value_balance.deferred_amount()),
        ]
    }
}
