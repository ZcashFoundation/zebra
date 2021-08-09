//! A type that can hold the four types of Zcash value pools.

use crate::amount::{Amount, Constraint, Error, NegativeAllowed, NonNegative};

use std::convert::TryInto;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

#[cfg(test)]
mod tests;

use ValueBalanceError::*;

/// An amount spread between different Zcash pools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        // Calculated in Zebra by negating the sum of the transparent, sprout,
        // sapling, and orchard value balances as specified in
        // https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions
        let value = (self.transparent + self.sprout + self.sapling + self.orchard)?;
        (-(value)).constrain::<NonNegative>()
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
            transparent: self.transparent.constrain().map_err(Transparent)?,
            sprout: self.sprout.constrain().map_err(Sprout)?,
            sapling: self.sapling.constrain().map_err(Sapling)?,
            orchard: self.orchard.constrain().map_err(Orchard)?,
        })
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
    pub fn from_bytes(bytes: [u8; 32]) -> Result<ValueBalance<C>, ValueBalanceError> {
        let transparent = Amount::from_bytes(
            bytes[0..8]
                .try_into()
                .expect("Extracting the first quarter of a [u8; 32] should always succeed"),
        )
        .map_err(Transparent)?;

        let sprout = Amount::from_bytes(
            bytes[8..16]
                .try_into()
                .expect("Extracting the second quarter of a [u8; 32] should always succeed"),
        )
        .map_err(Sprout)?;

        let sapling = Amount::from_bytes(
            bytes[16..24]
                .try_into()
                .expect("Extracting the third quarter of a [u8; 32] should always succeed"),
        )
        .map_err(Sapling)?;

        let orchard = Amount::from_bytes(
            bytes[24..32]
                .try_into()
                .expect("Extracting the last quarter of a [u8; 32] should always succeed"),
        )
        .map_err(Orchard)?;

        Ok(ValueBalance {
            transparent,
            sprout,
            sapling,
            orchard,
        })
    }
}

#[derive(thiserror::Error, Debug, displaydoc::Display, Clone, PartialEq, Eq)]
/// Errors that can be returned when validating a [`ValueBalance`]
pub enum ValueBalanceError {
    /// transparent amount error {0}
    Transparent(Error),

    /// sprout amount error {0}
    Sprout(Error),

    /// sapling amount error {0}
    Sapling(Error),

    /// orchard amount error {0}
    Orchard(Error),
}

impl<C> std::ops::Add for ValueBalance<C>
where
    C: Constraint,
{
    type Output = Result<ValueBalance<C>, ValueBalanceError>;
    fn add(self, rhs: ValueBalance<C>) -> Self::Output {
        Ok(ValueBalance::<C> {
            transparent: (self.transparent + rhs.transparent).map_err(Transparent)?,
            sprout: (self.sprout + rhs.sprout).map_err(Sprout)?,
            sapling: (self.sapling + rhs.sapling).map_err(Sapling)?,
            orchard: (self.orchard + rhs.orchard).map_err(Orchard)?,
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
            transparent: (self.transparent - rhs.transparent).map_err(Transparent)?,
            sprout: (self.sprout - rhs.sprout).map_err(Sprout)?,
            sapling: (self.sapling - rhs.sapling).map_err(Sapling)?,
            orchard: (self.orchard - rhs.orchard).map_err(Orchard)?,
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
            acc + value_balance
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

impl<C> std::ops::Neg for ValueBalance<C>
where
    C: Constraint,
{
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
