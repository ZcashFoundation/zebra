//! Randomised proptests for scanner database formats.

use proptest::prelude::*;

use crate::{
    service::finalized_state::arbitrary::assert_value_properties, SaplingScannedDatabaseIndex,
    SaplingScannedResult, SaplingScanningKey, MAX_ON_DISK_HEIGHT,
};

#[test]
fn roundtrip_sapling_scanning_key() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<SaplingScanningKey>())| assert_value_properties(val));
}

#[test]
fn roundtrip_sapling_db_index() {
    let _init_guard = zebra_test::init();

    proptest!(
        |(mut val in any::<SaplingScannedDatabaseIndex>())| {
            // Limit the random height to the valid on-disk range.
            // Blocks outside this range are rejected before they reach the state.
            // (It would take decades to generate a valid chain this high.)
            val.tx_loc.height.0 %= MAX_ON_DISK_HEIGHT.0 + 1;
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_option_sapling_result() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<Option<SaplingScannedResult>>())| assert_value_properties(val));
}
