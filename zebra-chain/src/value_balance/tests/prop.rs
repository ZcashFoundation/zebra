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
        let ironwood = value_balance1.ironwood + value_balance2.ironwood;

        match (transparent, sprout, sapling, orchard, deferred, ironwood) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard), Ok(deferred), Ok(ironwood)) => prop_assert_eq!(
                value_balance1 + value_balance2,
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood
                })
            ),
            _ => prop_assert!(
                matches!(
                    value_balance1 + value_balance2,
                    Err(ValueBalanceError::Transparent(_)
                        | ValueBalanceError::Sprout(_)
                        | ValueBalanceError::Sapling(_)
                        | ValueBalanceError::Orchard(_)
                        | ValueBalanceError::Deferred(_)
                        | ValueBalanceError::Ironwood(_))
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
        let ironwood = value_balance1.ironwood - value_balance2.ironwood;

        match (transparent, sprout, sapling, orchard, deferred, ironwood) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard), Ok(deferred), Ok(ironwood)) => prop_assert_eq!(
                value_balance1 - value_balance2,
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood
                })
            ),
            _ => prop_assert!(matches!(
                    value_balance1 - value_balance2,
                    Err(ValueBalanceError::Transparent(_)
                        | ValueBalanceError::Sprout(_)
                        | ValueBalanceError::Sapling(_)
                        | ValueBalanceError::Orchard(_)
                        | ValueBalanceError::Deferred(_)
                        | ValueBalanceError::Ironwood(_))
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
        let ironwood = value_balance1.ironwood + value_balance2.ironwood;

        match (transparent, sprout, sapling, orchard, deferred, ironwood) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard), Ok(deferred), Ok(ironwood)) => prop_assert_eq!(
                collection.iter().sum::<Result<ValueBalance<NegativeAllowed>, ValueBalanceError>>(),
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood
                })
            ),
            _ => prop_assert!(matches!(
                    collection.iter().sum(),
                    Err(ValueBalanceError::Transparent(_)
                        | ValueBalanceError::Sprout(_)
                        | ValueBalanceError::Sapling(_)
                        | ValueBalanceError::Orchard(_)
                        | ValueBalanceError::Deferred(_)
                        | ValueBalanceError::Ironwood(_))
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
    fn value_balance_deserialization(bytes in any::<[u8; 48]>()) {
        let _init_guard = zebra_test::init();

        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(&bytes) {
            prop_assert_eq!(bytes, deserialized.to_bytes());
        }
    }

    /// Earlier versions of [`ValueBalance`] had 32 bytes (no `deferred`) and then 40 bytes (no
    /// `ironwood`), compared to the current 48 bytes. It's possible to correctly instantiate the
    /// current version from either legacy format, with the missing trailing pools defaulting to
    /// zero, so we test that Zebra can still deserialize both legacy formats.
    #[test]
    fn legacy_value_balance_deserialization(
        bytes_32 in any::<[u8; 32]>(),
        bytes_40 in any::<[u8; 40]>(),
    ) {
        let _init_guard = zebra_test::init();

        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(&bytes_32) {
            let deserialized = deserialized.to_bytes();
            let mut extended_bytes = [0u8; 48];
            extended_bytes[..32].copy_from_slice(&bytes_32);
            prop_assert_eq!(extended_bytes, deserialized);
        }

        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(&bytes_40) {
            let deserialized = deserialized.to_bytes();
            let mut extended_bytes = [0u8; 48];
            extended_bytes[..40].copy_from_slice(&bytes_40);
            prop_assert_eq!(extended_bytes, deserialized);
        }
    }

}
