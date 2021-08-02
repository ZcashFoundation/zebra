//! A type that can hold the four types of Zcash value pools.

use crate::amount::{Amount, Constraint, Error, NonNegative};

use std::convert::TryInto;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

#[cfg(test)]
mod tests;

/// An amount spread between different Zcash pools.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueBalance<C> {
    transparent: Amount<C>,
    sprout: Amount<C>,
    sapling: Amount<C>,
    orchard: Amount<C>,
}

impl<C> ValueBalance<C>
where
    C: Constraint + Copy,
{
    /// [Consensus rule]: The remaining value in the transparent transaction value pool MUST
    /// be nonnegative.
    ///
    /// This rule applies to Block and Mempool transactions.
    ///
    /// [Consensus rule]: https://zips.z.cash/protocol/protocol.pdf#transactions
    pub fn remaining_transaction_value(&self) -> Result<Amount<NonNegative>, Error> {
        // This rule checks the transparent value balance minus the sum of the sprout,
        // sapling, and orchard value balances in a transaction is nonnegative.
        (self.transparent - (self.sprout + self.sapling + self.orchard)?)?
            .constrain::<NonNegative>()
    }

    /// Creates a [`ValueBalance`] from the given transparent amount.
    pub fn from_transparent_amount(transparent_amount: Amount<C>) -> Self {
        ValueBalance {
            transparent: transparent_amount,
            ..ValueBalance::zero()
        }
    }

    /// Creates a [`ValueBalance`] from the given sprout amount.
    pub fn from_sprout_amount(sprout_amount: Amount<C>) -> Self {
        ValueBalance {
            sprout: sprout_amount,
            ..ValueBalance::zero()
        }
    }

    /// Creates a [`ValueBalance`] from the given sapling amount.
    pub fn from_sapling_amount(sapling_amount: Amount<C>) -> Self {
        ValueBalance {
            sapling: sapling_amount,
            ..ValueBalance::zero()
        }
    }

    /// Creates a [`ValueBalance`] from the given orchard amount.
    pub fn from_orchard_amount(orchard_amount: Amount<C>) -> Self {
        ValueBalance {
            orchard: orchard_amount,
            ..ValueBalance::zero()
        }
    }

    /// Get the transparent amount from the [`ValueBalance`].
    pub fn transparent_amount(&self) -> Amount<C> {
        self.transparent
    }

    /// Insert a transparent value balance into a given [`ValueBalance`]
    /// leaving the other values untouched.
    pub fn set_transparent_value_balance(
        &mut self,
        transparent_value_balance: ValueBalance<C>,
    ) -> &Self {
        self.transparent = transparent_value_balance.transparent;
        self
    }

    /// Get the sprout amount from the [`ValueBalance`].
    pub fn sprout_amount(&self) -> Amount<C> {
        self.sprout
    }

    /// Insert a sprout value balance into a given [`ValueBalance`]
    /// leaving the other values untouched.
    pub fn set_sprout_value_balance(&mut self, sprout_value_balance: ValueBalance<C>) -> &Self {
        self.sprout = sprout_value_balance.sprout;
        self
    }

    /// Get the sapling amount from the [`ValueBalance`].
    pub fn sapling_amount(&self) -> Amount<C> {
        self.sapling
    }

    /// Insert a sapling value balance into a given [`ValueBalance`]
    /// leaving the other values untouched.
    pub fn set_sapling_value_balance(&mut self, sapling_value_balance: ValueBalance<C>) -> &Self {
        self.sapling = sapling_value_balance.sapling;
        self
    }

    /// Get the orchard amount from the [`ValueBalance`].
    pub fn orchard_amount(&self) -> Amount<C> {
        self.orchard
    }

    /// Insert an orchard value balance into a given [`ValueBalance`]
    /// leaving the other values untouched.
    pub fn set_orchard_value_balance(&mut self, orchard_value_balance: ValueBalance<C>) -> &Self {
        self.orchard = orchard_value_balance.orchard;
        self
    }

    /// Creates a [`ValueBalance`] where all the pools are zero.
    pub fn zero() -> Self {
        let zero = Amount::zero();
        Self {
            transparent: zero,
            sprout: zero,
            sapling: zero,
            orchard: zero,
        }
    }

    /// To byte array
    pub fn to_bytes(self) -> [u8; 32] {
        let transparent = self.transparent.to_bytes();
        let sprout = self.sprout.to_bytes();
        let sapling = self.sapling.to_bytes();
        let orchard = self.orchard.to_bytes();
        match [transparent, sprout, sapling, orchard].concat().try_into() {
            Ok(bytes) => bytes,
            _ => unreachable!(
                "Four [u8; 8] should always concat with no error into a single [u8; 32]"
            ),
        }
    }

    /// From byte array
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        let transparent = Amount::from_bytes(
            bytes[0..8]
                .try_into()
                .expect("Extracting the first quarter of a [u8; 32] should always succeed"),
        );
        let sprout = Amount::from_bytes(
            bytes[8..16]
                .try_into()
                .expect("Extracting the second quarter of a [u8; 32] should always succeed"),
        );
        let sapling = Amount::from_bytes(
            bytes[16..24]
                .try_into()
                .expect("Extracting the third quarter of a [u8; 32] should always succeed"),
        );
        let orchard = Amount::from_bytes(
            bytes[24..32]
                .try_into()
                .expect("Extracting the last quarter of a [u8; 32] should always succeed"),
        );

        ValueBalance {
            transparent,
            sprout,
            sapling,
            orchard,
        }
    }
}

#[derive(thiserror::Error, Debug, Clone, PartialEq)]
/// Errors that can be returned when validating a [`ValueBalance`].
pub enum ValueBalanceError {
    #[error("value balance contains invalid amounts")]
    /// Any error related to [`Amount`]s inside the [`ValueBalance`]
    AmountError(#[from] Error),
}

impl<C> std::ops::Add for ValueBalance<C>
where
    C: Constraint,
{
    type Output = Result<ValueBalance<C>, ValueBalanceError>;
    fn add(self, rhs: ValueBalance<C>) -> Self::Output {
        Ok(ValueBalance::<C> {
            transparent: (self.transparent + rhs.transparent)?,
            sprout: (self.sprout + rhs.sprout)?,
            sapling: (self.sapling + rhs.sapling)?,
            orchard: (self.orchard + rhs.orchard)?,
        })
    }
}
impl<C> std::ops::Add<ValueBalance<C>> for Result<ValueBalance<C>, ValueBalanceError>
where
    C: Constraint,
{
    type Output = Result<ValueBalance<C>, ValueBalanceError>;
    fn add(self, rhs: ValueBalance<C>) -> Self::Output {
        self? + rhs
    }
}

impl<C> std::ops::Sub for ValueBalance<C>
where
    C: Constraint,
{
    type Output = Result<ValueBalance<C>, ValueBalanceError>;
    fn sub(self, rhs: ValueBalance<C>) -> Self::Output {
        Ok(ValueBalance::<C> {
            transparent: (self.transparent - rhs.transparent)?,
            sprout: (self.sprout - rhs.sprout)?,
            sapling: (self.sapling - rhs.sapling)?,
            orchard: (self.orchard - rhs.orchard)?,
        })
    }
}
impl<C> std::ops::Sub<ValueBalance<C>> for Result<ValueBalance<C>, ValueBalanceError>
where
    C: Constraint,
{
    type Output = Result<ValueBalance<C>, ValueBalanceError>;
    fn sub(self, rhs: ValueBalance<C>) -> Self::Output {
        self? - rhs
    }
}

impl<C> std::iter::Sum<ValueBalance<C>> for Result<ValueBalance<C>, ValueBalanceError>
where
    C: Constraint + Copy,
{
    fn sum<I: Iterator<Item = ValueBalance<C>>>(mut iter: I) -> Self {
        iter.try_fold(ValueBalance::zero(), |acc, value_balance| {
            Ok(ValueBalance {
                transparent: (acc.transparent + value_balance.transparent)?,
                sprout: (acc.sprout + value_balance.sprout)?,
                sapling: (acc.sapling + value_balance.sapling)?,
                orchard: (acc.orchard + value_balance.orchard)?,
            })
        })
    }
}

impl<'amt, C> std::iter::Sum<&'amt ValueBalance<C>> for Result<ValueBalance<C>, ValueBalanceError>
where
    C: Constraint + std::marker::Copy + 'amt,
{
    fn sum<I: Iterator<Item = &'amt ValueBalance<C>>>(iter: I) -> Self {
        iter.copied().sum()
    }
}
