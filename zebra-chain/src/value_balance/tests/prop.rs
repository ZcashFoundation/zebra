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
        #[cfg(zcash_unstable = "zip234")]
        let Ok(nsm) = value_balance1.nsm + value_balance2.nsm else {
            prop_assert!((value_balance1 + value_balance2).is_err());
            return Ok(());
        };

        match (transparent, sprout, sapling, orchard, deferred, ironwood) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard), Ok(deferred), Ok(ironwood)) => prop_assert_eq!(
                value_balance1 + value_balance2,
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood,
                    #[cfg(zcash_unstable = "zip234")]
                    nsm,
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
        #[cfg(zcash_unstable = "zip234")]
        let Ok(nsm) = value_balance1.nsm - value_balance2.nsm else {
            prop_assert!((value_balance1 - value_balance2).is_err());
            return Ok(());
        };

        match (transparent, sprout, sapling, orchard, deferred, ironwood) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard), Ok(deferred), Ok(ironwood)) => prop_assert_eq!(
                value_balance1 - value_balance2,
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood,
                    #[cfg(zcash_unstable = "zip234")]
                    nsm,
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
        #[cfg(zcash_unstable = "zip234")]
        let Ok(nsm) = value_balance1.nsm + value_balance2.nsm else {
            prop_assert!(collection
                .iter()
                .sum::<Result<ValueBalance<NegativeAllowed>, ValueBalanceError>>()
                .is_err());
            return Ok(());
        };

        match (transparent, sprout, sapling, orchard, deferred, ironwood) {
            (Ok(transparent), Ok(sprout), Ok(sapling), Ok(orchard), Ok(deferred), Ok(ironwood)) => prop_assert_eq!(
                collection.iter().sum::<Result<ValueBalance<NegativeAllowed>, ValueBalanceError>>(),
                Ok(ValueBalance {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood,
                    #[cfg(zcash_unstable = "zip234")]
                    nsm,
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
    fn value_balance_deserialization(bytes in any::<[u8; SERIALIZED_SIZE]>()) {
        let _init_guard = zebra_test::init();

        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(&bytes) {
            prop_assert_eq!(bytes, deserialized.to_bytes());
        }
    }

    /// Earlier versions of [`ValueBalance`] had 32 bytes (no `deferred`), 40 bytes (no
    /// `ironwood`), and, with `zcash_unstable = "zip234"`, 48 bytes (no `nsm`), compared to the
    /// current [`SERIALIZED_SIZE`]. It's possible to correctly instantiate the current version
    /// from any legacy format, with the missing trailing pools defaulting to zero, so we test that
    /// Zebra can still deserialize the legacy formats.
    #[test]
    fn legacy_value_balance_deserialization(
        bytes_32 in any::<[u8; 32]>(),
        bytes_40 in any::<[u8; 40]>(),
        bytes_48 in any::<[u8; 48]>(),
    ) {
        let _init_guard = zebra_test::init();

        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(&bytes_32) {
            let deserialized = deserialized.to_bytes();
            let mut extended_bytes = [0u8; SERIALIZED_SIZE];
            extended_bytes[..32].copy_from_slice(&bytes_32);
            prop_assert_eq!(extended_bytes, deserialized);
        }

        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(&bytes_40) {
            let deserialized = deserialized.to_bytes();
            let mut extended_bytes = [0u8; SERIALIZED_SIZE];
            extended_bytes[..40].copy_from_slice(&bytes_40);
            prop_assert_eq!(extended_bytes, deserialized);
        }

        // The 48-byte format is the current one without `zcash_unstable = "zip234"`, so this is
        // only a legacy format with it.
        #[cfg(zcash_unstable = "zip234")]
        if let Ok(deserialized) = ValueBalance::<NonNegative>::from_bytes(&bytes_48) {
            let deserialized = deserialized.to_bytes();
            let mut extended_bytes = [0u8; SERIALIZED_SIZE];
            extended_bytes[..48].copy_from_slice(&bytes_48);
            prop_assert_eq!(extended_bytes, deserialized);
        }
        #[cfg(not(zcash_unstable = "zip234"))]
        let _ = bytes_48;
    }

}
