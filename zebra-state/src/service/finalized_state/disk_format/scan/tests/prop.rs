//! Randomised proptests for scanner database formats.

use proptest::{arbitrary::any, prelude::*};

use crate::{
    service::finalized_state::arbitrary::assert_value_properties, SaplingScannedDatabaseIndex,
    SaplingScannedResult, SaplingScanningKey,
};

#[test]
fn roundtrip_sapling_scanning_key() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<SaplingScanningKey>())| assert_value_properties(val));
}

#[test]
fn roundtrip_sapling_db_index() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<SaplingScannedDatabaseIndex>())| assert_value_properties(val));
}

#[test]
fn roundtrip_sapling_result() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<SaplingScannedResult>())| assert_value_properties(val));
}

#[test]
fn roundtrip_option_sapling_result() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<Option<SaplingScannedResult>>())| assert_value_properties(val));
}
