//! Randomised property tests for value balances.

use proptest::prelude::*;

use crate::{amount::*, value_balance::*};

proptest! {
    #[test]
    fn value_blance_add(
        value_balance1 in any::<ValueBalance<NegativeAllowed>>(),
        value_balance2 in any::<ValueBalance<NegativeAllowed>>())
    {
        let _init_guard = zebra_test::init();

        let transparent = value_balance1.transparent + value_balance2.transparent;
        let sprout = value_balance1.sprout + value_balance2.sprout;
        let sapling = value_balance1.sapling + value_balance2.sapling;
        let orchard = value_balance1.orchard + value_balance2.orchard;
        let deferred = value_balance1.deferred + value_balance2.deferred;


        match (transparent, sprout, sapling, orchard, deferred) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard), Ok(deferred)) => prop_assert_eq!(
                value_balance1 + value_balance2,
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred
                })
            ),
            _ => prop_assert!(
                matches!(
                    value_balance1 + value_balance2,
                    Err(ValueBalanceError::Transparent(_)
                        | ValueBalanceError::Sprout(_)
                        | ValueBalanceError::Sapling(_)
                        | ValueBalanceError::Orchard(_)
                        | ValueBalanceError::Deferred(_))
                )
            ),
        }
    }
    #[test]
    fn value_balance_sub(
        value_balance1 in any::<ValueBalance<NegativeAllowed>>(),
        value_balance2 in any::<ValueBalance<NegativeAllowed>>())
    {
        let _init_guard = zebra_test::init();

        let transparent = value_balance1.transparent - value_balance2.transparent;
        let sprout = value_balance1.sprout - value_balance2.sprout;
        let sapling = value_balance1.sapling - value_balance2.sapling;
        let orchard = value_balance1.orchard - value_balance2.orchard;
        let deferred = value_balance1.deferred - value_balance2.deferred;

        match (transparent, sprout, sapling, orchard, deferred) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard), Ok(deferred)) => prop_assert_eq!(
                value_balance1 - value_balance2,
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred
                })
            ),
            _ => prop_assert!(matches!(
                    value_balance1 - value_balance2,
                    Err(ValueBalanceError::Transparent(_)
                        | ValueBalanceError::Sprout(_)
                        | ValueBalanceError::Sapling(_)
                        | ValueBalanceError::Orchard(_)
                        | ValueBalanceError::Deferred(_))
                )),
        }
    }

    #[test]
    fn value_balance_sum(
        value_balance1 in any::<ValueBalance<NegativeAllowed>>(),
        value_balance2 in any::<ValueBalance<NegativeAllowed>>(),
    ) {
        let _init_guard = zebra_test::init();

        let collection = [value_balance1, value_balance2];

        let transparent = value_balance1.transparent + value_balance2.transparent;
        let sprout = value_balance1.sprout + value_balance2.sprout;
        let sapling = value_balance1.sapling + value_balance2.sapling;
        let orchard = value_balance1.orchard + value_balance2.orchard;
        let deferred = value_balance1.deferred + value_balance2.deferred;

        match (transparent, sprout, sapling, orchard, deferred) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard), Ok(deferred)) => prop_assert_eq!(
                collection.iter().sum::<Result<ValueBalance<NegativeAllowed>, ValueBalanceError>>(),
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred
                })
            ),
            _ => prop_assert!(matches!(
                    collection.iter().sum(),
                    Err(ValueBalanceError::Transparent(_)
                        | ValueBalanceError::Sprout(_)
                        | ValueBalanceError::Sapling(_)
                        | ValueBalanceError::Orchard(_)
                        | ValueBalanceError::Deferred(_))
                 ))
        }
    }

    #[test]
    fn value_balance_serialization(value_balance in any::<ValueBalance<NonNegative>>()) {
        let _init_guard = zebra_test::init();

        let serialized_value_balance = ValueBalance::from_bytes(&value_balance.to_bytes())?;

        prop_assert_eq!(value_balance, serialized_value_balance);
    }

    #[test]
    fn value_balance_deserialization(bytes in any::<[u8; 40]>()) {
        let _init_guard = zebra_test::init();

        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(&bytes) {
            prop_assert_eq!(bytes, deserialized.to_bytes());
        }
    }

    /// The legacy version of [`ValueBalance`] had 32 bytes compared to the current 40 bytes,
    /// but it's possible to correctly instantiate the current version of [`ValueBalance`] from
    /// the legacy format, so we test if Zebra can still deserialiaze the legacy format.
    #[test]
    fn legacy_value_balance_deserialization(bytes in any::<[u8; 32]>()) {
        let _init_guard = zebra_test::init();

        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(&bytes) {
            let deserialized = deserialized.to_bytes();
            let mut extended_bytes = [0u8; 40];
            extended_bytes[..32].copy_from_slice(&bytes);
            prop_assert_eq!(extended_bytes, deserialized);
        }
    }

}
