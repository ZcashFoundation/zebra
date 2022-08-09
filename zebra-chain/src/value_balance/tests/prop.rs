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
                    value_balance1 + value_balance2,
                    Err(ValueBalanceError::Transparent(_)
                        | ValueBalanceError::Sprout(_)
                        | ValueBalanceError::Sapling(_)
                        | ValueBalanceError::Orchard(_))
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
                    value_balance1 - value_balance2,
                    Err(ValueBalanceError::Transparent(_)
                        | ValueBalanceError::Sprout(_)
                        | ValueBalanceError::Sapling(_)
                        | ValueBalanceError::Orchard(_))
                )
            ),
        }
    }

    #[test]
    fn value_balance_sum(
        value_balance1 in any::<ValueBalance<NegativeAllowed>>(),
        value_balance2 in any::<ValueBalance<NegativeAllowed>>(),
    ) {
        let _init_guard = zebra_test::init();

        let collection = vec![value_balance1, value_balance2];

        let transparent = value_balance1.transparent + value_balance2.transparent;
        let sprout = value_balance1.sprout + value_balance2.sprout;
        let sapling = value_balance1.sapling + value_balance2.sapling;
        let orchard = value_balance1.orchard + value_balance2.orchard;

        match (transparent, sprout, sapling, orchard) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard)) => prop_assert_eq!(
                collection.iter().sum::<Result<ValueBalance<NegativeAllowed>, ValueBalanceError>>(),
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                })
            ),
            _ => prop_assert!(matches!(collection.iter().sum(),
                                       Err(ValueBalanceError::Transparent(_)
                                           | ValueBalanceError::Sprout(_)
                                           | ValueBalanceError::Sapling(_)
                                           | ValueBalanceError::Orchard(_))
            ))
        }
    }

    #[test]
    fn value_balance_serialization(value_balance in any::<ValueBalance<NonNegative>>()) {
        let _init_guard = zebra_test::init();

        let bytes = value_balance.to_bytes();
        let serialized_value_balance = ValueBalance::from_bytes(bytes)?;

        prop_assert_eq!(value_balance, serialized_value_balance);
    }

    #[test]
    fn value_balance_deserialization(bytes in any::<[u8; 32]>()) {
        let _init_guard = zebra_test::init();

        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(bytes) {
            let bytes2 = deserialized.to_bytes();
            prop_assert_eq!(bytes, bytes2);
        }
    }
}
