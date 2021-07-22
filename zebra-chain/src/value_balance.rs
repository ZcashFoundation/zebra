//! A type that can hold the four types of Zcash value pools.

use crate::amount::{Amount, Constraint, Error, NegativeAllowed, NonNegative};

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

    fn zero() -> Self {
        let zero = Amount::zero();
        Self {
            transparent: zero,
            sprout: zero,
            sapling: zero,
            orchard: zero,
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

#[cfg(any(test, feature = "proptest-impl"))]
use proptest::prelude::*;
#[cfg(any(test, feature = "proptest-impl"))]
impl Arbitrary for ValueBalance<NegativeAllowed> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Amount>(),
            any::<Amount>(),
            any::<Amount>(),
            any::<Amount>(),
        )
            .prop_map(|(transparent, sprout, sapling, orchard)| Self {
                transparent,
                sprout,
                sapling,
                orchard,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(any(test, feature = "proptest-impl"))]
impl Arbitrary for ValueBalance<NonNegative> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
        )
            .prop_map(|(transparent, sprout, sapling, orchard)| Self {
                transparent,
                sprout,
                sapling,
                orchard,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod test {
    use super::*;

    proptest! {
        #[test]
        fn test_add(
            value_balance1 in any::<ValueBalance<NegativeAllowed>>(),
            value_balance2 in any::<ValueBalance<NegativeAllowed>>())
        {
            zebra_test::init();

            let transparent = value_balance1.transparent + value_balance2.transparent;
            let sprout = value_balance1.sprout + value_balance2.sprout;
            let sapling = value_balance1.sapling + value_balance2.sapling;
            let orchard = value_balance1.orchard + value_balance2.orchard;

            match (transparent, sprout, sapling, orchard) {
                (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard)) => prop_assert_eq!(
                    value_balance1 + value_balance2,
                    Ok(ValueBalance {
                        transparent,
                        sprout,
                        sapling,
                        orchard,
                    })
                ),
                _ => prop_assert!(
                    matches!(
                        value_balance1 + value_balance2, Err(ValueBalanceError::AmountError(_))
                    )
                ),
            }
        }
        #[test]
        fn test_sub(
            value_balance1 in any::<ValueBalance<NegativeAllowed>>(),
            value_balance2 in any::<ValueBalance<NegativeAllowed>>())
        {
            zebra_test::init();

            let transparent = value_balance1.transparent - value_balance2.transparent;
            let sprout = value_balance1.sprout - value_balance2.sprout;
            let sapling = value_balance1.sapling - value_balance2.sapling;
            let orchard = value_balance1.orchard - value_balance2.orchard;

            match (transparent, sprout, sapling, orchard) {
                (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard)) => prop_assert_eq!(
                    value_balance1 - value_balance2,
                    Ok(ValueBalance {
                        transparent,
                        sprout,
                        sapling,
                        orchard,
                    })
                ),
                _ => prop_assert!(
                    matches!(
                        value_balance1 - value_balance2, Err(ValueBalanceError::AmountError(_))
                    )
                ),
            }
        }
    }
}
