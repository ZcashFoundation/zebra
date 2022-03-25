//! Randomised tests for the finalized disk format.

use proptest::{arbitrary::any, prelude::*};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Height},
    transparent,
    value_balance::ValueBalance,
};

use crate::service::finalized_state::{
    arbitrary::assert_value_properties,
    disk_format::{
        block::MAX_ON_DISK_HEIGHT,
        transparent::{AddressBalanceLocation, AddressLocation, OutputLocation},
        IntoDisk, TransactionLocation,
    },
};

#[test]
fn serialized_transparent_address_equal() {
    zebra_test::init();
    proptest!(|(addr1 in any::<transparent::Address>(), addr2 in any::<transparent::Address>())| {
        if addr1 == addr2 {
            prop_assert_eq!(
                addr1.as_bytes(),
                addr2.as_bytes(),
                "transparent addresses were equal, but serialized bytes were not.\n\
                 Addresses:\n\
                 {:?}\n\
                 {:?}",
                addr1,
                addr2,
            );
        } else {
            prop_assert_ne!(
                addr1.as_bytes(),
                addr2.as_bytes(),
                "transparent addresses were not equal, but serialized bytes were equal:\n\
                 Addresses:\n\
                 {:?}\n\
                 {:?}",
                addr1,
                addr2,
            );
        }
    }
    );
}

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
fn roundtrip_output_location() {
    zebra_test::init();
    proptest!(|(val in any::<OutputLocation>())| assert_value_properties(val));
}

#[test]
fn roundtrip_address_location() {
    zebra_test::init();
    proptest!(|(val in any::<AddressLocation>())| assert_value_properties(val));
}

#[test]
fn roundtrip_address_balance_location() {
    zebra_test::init();
    proptest!(|(val in any::<AddressBalanceLocation>())| assert_value_properties(val));
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

#[test]
fn roundtrip_transparent_output() {
    zebra_test::init();

    proptest!(
        |(mut val in any::<transparent::Utxo>())| {
            val.height = val.height.clamp(Height(0), MAX_ON_DISK_HEIGHT);
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_amount() {
    zebra_test::init();

    proptest!(|(val in any::<Amount::<NonNegative>>())| assert_value_properties(val));
}

#[test]
fn roundtrip_value_balance() {
    zebra_test::init();

    proptest!(|(val in any::<ValueBalance::<NonNegative>>())| assert_value_properties(val));
}
