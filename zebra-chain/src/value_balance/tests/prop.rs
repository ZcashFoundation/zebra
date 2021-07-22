use crate::{amount::*, value_balance::*};
use proptest::prelude::*;

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
