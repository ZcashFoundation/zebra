//! A type that can hold the four types of Zcash value pools.

use crate::amount::{self, Amount, Constraint, NegativeAllowed, NonNegative};

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
    /// Design: https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/rfcs/0012-value-pools.md#definitions
    //
    // TODO: move this method to Transaction, and assert that the transaction is not coinbase
    //       for coinbase transactions, create a method with two extra arguments:
    //       block subsidy, and miner fees
    pub fn remaining_transaction_value(&self) -> Result<Amount<NonNegative>, ValueBalanceError> {
        [self.transparent, self.sprout, self.sapling, self.orchard]
            .iter()
            .sum::<Result<Amount<C>, amount::Error>>()?
            .constrain::<NonNegative>()
            .map_err(Into::into)
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

    /// Convert this value balance to a different ValueBalance type,
    /// if it satisfies the new constraint
    pub fn constrain<C2>(self) -> Result<ValueBalance<C2>, ValueBalanceError>
    where
        C2: Constraint,
    {
        Ok(ValueBalance::<C2> {
            transparent: self.transparent.constrain()?,
            sprout: self.sprout.constrain()?,
            sapling: self.sapling.constrain()?,
            orchard: self.orchard.constrain()?,
        })
    }
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
/// Errors that can be returned when validating a [`ValueBalance`].
pub enum ValueBalanceError {
    #[error("value balance contains invalid amounts")]
    /// Any error related to [`Amount`]s inside the [`ValueBalance`]
    AmountError(#[from] amount::Error),
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

impl<C> std::ops::Neg for ValueBalance<C> {
    type Output = ValueBalance<NegativeAllowed>;

    fn neg(self) -> Self::Output {
        ValueBalance::<NegativeAllowed> {
            transparent: self.transparent.neg(),
            sprout: self.sprout.neg(),
            sapling: self.sapling.neg(),
            orchard: self.orchard.neg(),
        }
    }
}
