//! A type that can hold the Zcash four types of value pools

use crate::amount::{Amount, Constraint, Error, NegativeAllowed, NonNegative};

/// A structure that hold amounts for the four value pool types.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "C: Constraint")]
pub struct ValueBalance<C = NegativeAllowed> {
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

    /// Creates a `ValueBalance` from the given transparent amount.
    pub fn from_transparent_amount(transparent_amount: Amount<C>) -> Self {
        ValueBalance {
            transparent: transparent_amount,
            ..Default::default()
        }
    }

    /// Creates a `ValueBalance` from the given sprout amount.
    pub fn from_sprout_amount(sprout_amount: Amount<C>) -> Self {
        ValueBalance {
            sprout: sprout_amount,
            ..Default::default()
        }
    }

    /// Creates a `ValueBalance` from the given sapling amount.
    pub fn from_sapling_amount(sapling_amount: Amount<C>) -> Self {
        ValueBalance {
            sapling: sapling_amount,
            ..Default::default()
        }
    }

    /// Creates a `ValueBalance` from the given orchard amount.
    pub fn from_orchard_amount(orchard_amount: Amount<C>) -> Self {
        ValueBalance {
            orchard: orchard_amount,
            ..Default::default()
        }
    }

    /// Insert a transparent value balance into a given `ValueBalance`
    /// leaving the other values untouched.
    pub fn from_transparent_value_balance(
        &mut self,
        transparent_value_balance: ValueBalance<C>,
    ) -> &Self {
        self.transparent = transparent_value_balance.transparent;
        self
    }

    /// Insert a sprout value balance into a given `ValueBalance`
    /// leaving the other values untouched.
    pub fn from_sprout_value_balance(&mut self, sprout_value_balance: ValueBalance<C>) -> &Self {
        self.sprout = sprout_value_balance.sprout;
        self
    }

    /// Insert a sapling value balance into a given `ValueBalance`
    /// leaving the other values untouched.
    pub fn from_sapling_value_balance(&mut self, sapling_value_balance: ValueBalance<C>) -> &Self {
        self.sapling = sapling_value_balance.sapling;
        self
    }

    /// Insert an orchard value balance into a given `ValueBalance`
    /// leaving the other values untouched.
    pub fn from_orchard_value_balance(&mut self, orchard_value_balance: ValueBalance<C>) -> &Self {
        self.orchard = orchard_value_balance.orchard;
        self
    }
}

impl<C> Default for ValueBalance<C>
where
    C: Constraint + Copy,
{
    fn default() -> Self {
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
//#[allow(missing_docs)]
/// Errors that can be returned when validating `ValueBalance`s
enum ValueBalanceError {
    #[error("value balance contains invalid amounts")]
    AmountError(#[from] crate::amount::Error),
}

impl<C> std::ops::Add<ValueBalance<C>> for Result<ValueBalance<C>, ValueBalanceError>
where
    C: Constraint,
{
    type Output = Result<ValueBalance<C>, ValueBalanceError>;

    fn add(self, rhs: ValueBalance<C>) -> Self::Output {
        let value_balance = self?;

        Ok(ValueBalance::<C> {
            transparent: (value_balance.transparent + rhs.transparent)?,
            sprout: (value_balance.sprout + rhs.sprout)?,
            sapling: (value_balance.sapling + rhs.sapling)?,
            orchard: (value_balance.orchard + rhs.orchard)?,
        })
    }
}

impl<C> std::ops::Sub<ValueBalance<C>> for Result<ValueBalance<C>, ValueBalanceError>
where
    C: Constraint,
{
    type Output = Result<ValueBalance<C>, ValueBalanceError>;

    fn sub(self, rhs: ValueBalance<C>) -> Self::Output {
        let value_balance = self?;

        Ok(ValueBalance::<C> {
            transparent: (value_balance.transparent - rhs.transparent)?,
            sprout: (value_balance.sprout - rhs.sprout)?,
            sapling: (value_balance.sapling - rhs.sapling)?,
            orchard: (value_balance.orchard - rhs.orchard)?,
        })
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
        fn test_operations(
            value_balance1 in any::<ValueBalance>(),
            value_balance2 in any::<ValueBalance>())
        {
            zebra_test::init();

            // test the add operator
            let add = Ok(value_balance1) + value_balance2;

            if !add.is_err() {
                prop_assert_eq!(add?, ValueBalance {
                    transparent: (value_balance1.transparent + value_balance2.transparent)?,
                    sprout: (value_balance1.sprout + value_balance2.sprout)?,
                    sapling: (value_balance1.sapling + value_balance2.sapling)?,
                    orchard: (value_balance1.orchard + value_balance2.orchard)?,
                });
            }
            else {
                let error_string = format!("{:?}", add.err().unwrap());
                prop_assert_eq!(error_string.contains("AmountError"), true);
            }

            // check the sub operator
            let sub = Ok(value_balance1) - value_balance2;

            if !sub.is_err() {
                prop_assert_eq!(sub?, ValueBalance {
                    transparent: (value_balance1.transparent - value_balance2.transparent)?,
                    sprout: (value_balance1.sprout - value_balance2.sprout)?,
                    sapling: (value_balance1.sapling - value_balance2.sapling)?,
                    orchard: (value_balance1.orchard - value_balance2.orchard)?,
                });
            }
            else {
                let error_string = format!("{:?}", sub.err().unwrap());
                prop_assert_eq!(error_string.contains("AmountError"), true);
            }
        }
    }
}
