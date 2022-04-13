//! Randomised tests for the finalized disk format.

use proptest::{arbitrary::any, prelude::*};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Height},
    orchard, sapling, sprout,
    transaction::{self, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use crate::service::finalized_state::{
    arbitrary::assert_value_properties,
    disk_format::{
        block::MAX_ON_DISK_HEIGHT,
        transparent::{
            AddressBalanceLocation, AddressLocation, AddressUnspentOutput, OutputLocation,
        },
        IntoDisk, TransactionLocation,
    },
};

// Common

// TODO: turn this into a unit test, it has a fixed value
#[test]
fn roundtrip_unit_type() {
    zebra_test::init();

    proptest!(|(val in any::<()>())| assert_value_properties(val));
}

// Block
// TODO: split these tests into the disk_format sub-modules

#[test]
fn roundtrip_block_height() {
    zebra_test::init();

    proptest!(
        |(mut val in any::<Height>())| {
            // Limit the random height to the valid on-disk range.
            // Blocks outside this range are rejected before they reach the state.
            // (It would take decades to generate a valid chain this high.)
            val = val.clamp(Height(0), MAX_ON_DISK_HEIGHT);
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_block_hash() {
    zebra_test::init();

    proptest!(|(val in any::<block::Hash>())| assert_value_properties(val));
}

#[test]
fn roundtrip_block_header() {
    zebra_test::init();

    proptest!(|(val in any::<block::Header>())| assert_value_properties(val));
}

// Transaction

#[test]
fn roundtrip_transaction_location() {
    zebra_test::init();

    proptest!(
        |(mut val in any::<TransactionLocation>())| {
            val.height = val.height.clamp(Height(0), MAX_ON_DISK_HEIGHT);
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_transaction_hash() {
    zebra_test::init();

    proptest!(|(val in any::<transaction::Hash>())| assert_value_properties(val));
}

#[test]
fn roundtrip_transaction() {
    zebra_test::init();

    proptest!(|(val in any::<Transaction>())| assert_value_properties(val));
}

// Transparent

// TODO: turn this into a generic function like assert_value_properties()
#[test]
fn serialized_transparent_address_equal() {
    zebra_test::init();

    proptest!(|(val1 in any::<transparent::Address>(), val2 in any::<transparent::Address>())| {
        if val1 == val2 {
            prop_assert_eq!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were equal, but serialized bytes were not.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        } else {
            prop_assert_ne!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were not equal, but serialized bytes were equal.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        }
    }
    );
}

#[test]
fn roundtrip_transparent_address() {
    zebra_test::init();

    proptest!(|(val in any::<transparent::Address>())| assert_value_properties(val));
}

#[test]
fn roundtrip_output_location() {
    zebra_test::init();

    proptest!(
        |(mut val in any::<OutputLocation>())| {
            *val.height_mut() = val.height().clamp(Height(0), MAX_ON_DISK_HEIGHT);
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_address_location() {
    zebra_test::init();

    proptest!(
        |(mut val in any::<AddressLocation>())| {
            *val.height_mut() = val.height().clamp(Height(0), MAX_ON_DISK_HEIGHT);
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_address_balance_location() {
    zebra_test::init();

    proptest!(
        |(mut val in any::<AddressBalanceLocation>())| {
            *val.height_mut() = val.address_location().height().clamp(Height(0), MAX_ON_DISK_HEIGHT);
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_transparent_output() {
    zebra_test::init();

    proptest!(|(val in any::<transparent::Output>())| assert_value_properties(val));
}

#[test]
fn roundtrip_address_unspent_output() {
    zebra_test::init();

    proptest!(
        |(mut val in any::<AddressUnspentOutput>())| {
            *val.address_location_mut().height_mut() = val.address_location().height().clamp(Height(0), MAX_ON_DISK_HEIGHT);
            *val.unspent_output_location_mut().height_mut() = val.unspent_output_location().height().clamp(Height(0), MAX_ON_DISK_HEIGHT);

            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_amount() {
    zebra_test::init();

    proptest!(|(val in any::<Amount::<NonNegative>>())| assert_value_properties(val));
}

// Sprout

#[test]
fn serialized_sprout_nullifier_equal() {
    zebra_test::init();

    proptest!(|(val1 in any::<sprout::Nullifier>(), val2 in any::<sprout::Nullifier>())| {
        if val1 == val2 {
            prop_assert_eq!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were equal, but serialized bytes were not.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        } else {
            prop_assert_ne!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were not equal, but serialized bytes were equal.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        }
    }
    );
}

#[test]
fn serialized_sprout_tree_root_equal() {
    zebra_test::init();

    proptest!(|(val1 in any::<sprout::tree::Root>(), val2 in any::<sprout::tree::Root>())| {
        if val1 == val2 {
            prop_assert_eq!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were equal, but serialized bytes were not.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        } else {
            prop_assert_ne!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were not equal, but serialized bytes were equal.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        }
    }
    );
}

// TODO: test note commitment tree round-trip, after implementing proptest::Arbitrary

// Sapling

#[test]
fn serialized_sapling_nullifier_equal() {
    zebra_test::init();

    proptest!(|(val1 in any::<sapling::Nullifier>(), val2 in any::<sapling::Nullifier>())| {
        if val1 == val2 {
            prop_assert_eq!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were equal, but serialized bytes were not.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        } else {
            prop_assert_ne!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were not equal, but serialized bytes were equal.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        }
    }
    );
}

#[test]
fn serialized_sapling_tree_root_equal() {
    zebra_test::init();

    proptest!(|(val1 in any::<sapling::tree::Root>(), val2 in any::<sapling::tree::Root>())| {
        if val1 == val2 {
            prop_assert_eq!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were equal, but serialized bytes were not.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        } else {
            prop_assert_ne!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were not equal, but serialized bytes were equal.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        }
    }
    );
}

// TODO: test note commitment tree round-trip, after implementing proptest::Arbitrary

// Orchard

#[test]
fn serialized_orchard_nullifier_equal() {
    zebra_test::init();

    proptest!(|(val1 in any::<orchard::Nullifier>(), val2 in any::<orchard::Nullifier>())| {
        if val1 == val2 {
            prop_assert_eq!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were equal, but serialized bytes were not.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        } else {
            prop_assert_ne!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were not equal, but serialized bytes were equal.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        }
    }
    );
}

#[test]
fn serialized_orchard_tree_root_equal() {
    zebra_test::init();

    proptest!(|(val1 in any::<orchard::tree::Root>(), val2 in any::<orchard::tree::Root>())| {
        if val1 == val2 {
            prop_assert_eq!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were equal, but serialized bytes were not.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        } else {
            prop_assert_ne!(
                val1.as_bytes(),
                val2.as_bytes(),
                "struct values were not equal, but serialized bytes were equal.\n\
                 Values:\n\
                 {:?}\n\
                 {:?}",
                val1,
                val2,
            );
        }
    }
    );
}

// TODO: test note commitment tree round-trip, after implementing proptest::Arbitrary

// Chain

// TODO: test NonEmptyHistoryTree round-trip, after implementing proptest::Arbitrary

#[test]
fn roundtrip_value_balance() {
    zebra_test::init();

    proptest!(|(val in any::<ValueBalance::<NonNegative>>())| assert_value_properties(val));
}
