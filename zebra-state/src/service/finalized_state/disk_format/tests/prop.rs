//! Randomised tests for the finalized disk format.

use proptest::{arbitrary::any, prelude::*};

use zebra_chain::{
    amount::NonNegative,
    block::{self, Block, Height},
    transparent,
    value_balance::ValueBalance,
};

use crate::service::finalized_state::{
    arbitrary::assert_value_properties,
    disk_format::{block::MAX_ON_DISK_HEIGHT, transparent::OutputLocation, TransactionLocation},
};

#[test]
fn roundtrip_block_height() {
    zebra_test::init();

    proptest!(
        |(mut val in any::<block::Height>())| {
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
fn roundtrip_block_hash() {
    zebra_test::init();
    proptest!(|(val in any::<block::Hash>())| assert_value_properties(val));
}

#[test]
fn roundtrip_block() {
    zebra_test::init();

    proptest!(|(val in any::<Block>())| assert_value_properties(val));
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
fn roundtrip_value_balance() {
    zebra_test::init();

    proptest!(|(val in any::<ValueBalance::<NonNegative>>())| assert_value_properties(val));
}
