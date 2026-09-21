//! Randomised tests for the finalized disk format.

use proptest::prelude::*;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Height},
    block_info::BlockInfo,
    orchard, sapling, sprout,
    subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction::{self, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use crate::service::finalized_state::{
    arbitrary::assert_value_properties,
    disk_format::{
        block::MAX_ON_DISK_HEIGHT,
        transparent::{
            AddressBalanceLocation, AddressLocation, AddressTransaction, AddressUnspentOutput,
            OutputLocation,
        },
        FromDisk, IntoDisk, TransactionLocation,
    },
};

// Common

/// This test has a fixed value, so testing it once is sufficient.
#[test]
fn roundtrip_unit_type() {
    let _init_guard = zebra_test::init();

    // The unit type `()` is serialized to the empty (zero-length) array `[]`.
    #[allow(clippy::let_unit_value)]
    let value = ();
    assert_value_properties(value);
}

// Block
// TODO: split these tests into the disk_format sub-modules

#[test]
fn roundtrip_block_height() {
    let _init_guard = zebra_test::init();

    proptest!(
        |(mut val in any::<Height>())| {
            // Limit the random height to the valid on-disk range.
            // Blocks outside this range are rejected before they reach the state.
            // (It would take decades to generate a valid chain this high.)
            val.0 %= MAX_ON_DISK_HEIGHT.0 + 1;
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_block_hash() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<block::Hash>())| assert_value_properties(val));
}

#[test]
fn roundtrip_block_header() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<block::Header>())| assert_value_properties(val));
}

// Transaction

#[test]
fn roundtrip_transaction_location() {
    let _init_guard = zebra_test::init();

    proptest!(
        |(mut val in any::<TransactionLocation>())| {
            val.height.0 %= MAX_ON_DISK_HEIGHT.0 + 1;
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_transaction_hash() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<transaction::Hash>())| assert_value_properties(val));
}

#[test]
fn roundtrip_transaction() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<Transaction>())| {
        // Coinbase transactions with Sapling spends are rejected during deserialization
        // (GHSA-rgwx-8r98-p34c), so they cannot round-trip through `IntoDisk`/`FromDisk`.
        // The arbitrary `Transaction` strategy still produces them so the
        // `transaction_roundtrip` proptest in `zebra-chain` can exercise the rejection path.
        prop_assume!(!(val.is_coinbase() && val.sapling_spends().count() > 0));
        assert_value_properties(val)
    });
}

// Transparent

// TODO: turn this into a generic function like assert_value_properties()
#[test]
fn serialized_transparent_address_equal() {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<transparent::Address>())| assert_value_properties(val));
}

#[test]
fn roundtrip_output_location() {
    let _init_guard = zebra_test::init();

    proptest!(
        |(mut val in any::<OutputLocation>())| {
            val.height_mut().0 %= MAX_ON_DISK_HEIGHT.0 + 1;
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_address_location() {
    let _init_guard = zebra_test::init();

    proptest!(
        |(mut val in any::<AddressLocation>())| {
            val.height_mut().0 %= MAX_ON_DISK_HEIGHT.0 + 1;
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_address_balance_location() {
    let _init_guard = zebra_test::init();

    proptest!(
        |(mut val in any::<AddressBalanceLocation>())| {
            val.height_mut().0 %= MAX_ON_DISK_HEIGHT.0 + 1;
            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_transparent_output() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<transparent::Output>())| assert_value_properties(val));
}

#[test]
fn roundtrip_address_unspent_output() {
    let _init_guard = zebra_test::init();

    proptest!(
        |(mut val in any::<AddressUnspentOutput>())| {
            val.address_location_mut().height_mut().0 %= MAX_ON_DISK_HEIGHT.0 + 1;
            val.unspent_output_location_mut().height_mut().0 %= MAX_ON_DISK_HEIGHT.0 + 1;

            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_address_transaction() {
    let _init_guard = zebra_test::init();

    proptest!(
        |(mut val in any::<AddressTransaction>())| {
            val.address_location_mut().height_mut().0 %= MAX_ON_DISK_HEIGHT.0 + 1;
            val.transaction_location_mut().height.0 %= MAX_ON_DISK_HEIGHT.0 + 1;

            assert_value_properties(val)
        }
    );
}

#[test]
fn roundtrip_amount() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<Amount::<NonNegative>>())| assert_value_properties(val));
}

#[test]
fn roundtrip_note_commitment_subtree_index() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<NoteCommitmentSubtreeIndex>())| {
        assert_value_properties(val)
    });
}

// Sprout

#[test]
fn serialized_sprout_nullifier_equal() {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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

#[test]
fn roundtrip_sprout_tree_root() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<sprout::tree::Root>())| assert_value_properties(val));
}

// TODO: test note commitment tree round-trip, after implementing proptest::Arbitrary

// Sapling

#[test]
fn serialized_sapling_nullifier_equal() {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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

#[test]
fn roundtrip_sapling_tree_root() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<sapling::tree::Root>())| assert_value_properties(val));
}

#[test]
fn roundtrip_sapling_subtree_data() {
    let _init_guard = zebra_test::init();

    proptest!(|(mut val in any::<NoteCommitmentSubtreeData<sapling::tree::legacy::Node>>())| {
        val.end_height.0 %= MAX_ON_DISK_HEIGHT.0 + 1;
        assert_value_properties(val.root.0)
    });
}

// TODO: test note commitment tree round-trip, after implementing proptest::Arbitrary

// Orchard

#[test]
fn serialized_orchard_nullifier_equal() {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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

#[test]
fn roundtrip_orchard_tree_root() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<orchard::tree::Root>())| assert_value_properties(val));
}

#[test]
fn roundtrip_orchard_subtree_data() {
    let _init_guard = zebra_test::init();

    proptest!(|(mut val in any::<NoteCommitmentSubtreeData<orchard::tree::Node>>())| {
        val.end_height.0 %= MAX_ON_DISK_HEIGHT.0 + 1;
        assert_value_properties(val)
    });
}

// TODO: test note commitment tree round-trip, after implementing proptest::Arbitrary

// Chain

// TODO: test NonEmptyHistoryTree round-trip, after implementing proptest::Arbitrary

#[test]
fn roundtrip_value_balance() {
    let _init_guard = zebra_test::init();

    proptest!(|(val in any::<ValueBalance::<NonNegative>>())| assert_value_properties(val));
}

#[test]
fn roundtrip_block_info() {
    let _init_guard = zebra_test::init();

    proptest!(
        |(value_pools in any::<ValueBalance<NonNegative>>(), size in any::<u32>())| {
            assert_value_properties(BlockInfo::new(value_pools, size))
        }
    );
}

/// [`BlockInfo`] records grew with each new chain value pool, so reading one must pick the layout
/// by length. Reading a current record with an older layout silently mis-parses the block size,
/// which only shows up far away from the disk format code, so pin every layout here.
#[test]
fn block_info_reads_every_record_layout() {
    let _init_guard = zebra_test::init();

    let size = 0x0000_069c_u32;
    let size_bytes = size.to_le_bytes();
    let nsm_reserve = Amount::<NonNegative>::try_from(60_001).expect("valid amount");
    let nsm_bytes = nsm_reserve.to_bytes();

    // A pre-NU6.3 record: a 40-byte value pool, then the size.
    let legacy_40 = [[0; 40].as_slice(), &size_bytes].concat();
    // A record with a zero NSM reserve (the v28.0 layout): a 48-byte value pool, then the size.
    let narrow = [[0; 48].as_slice(), &size_bytes].concat();
    // A record with a non-zero NSM reserve: the v28.0 fields, then the reserve.
    let wide = [[0; 48].as_slice(), &size_bytes, &nsm_bytes].concat();

    for (bytes, layout, expected_nsm_reserve) in [
        (legacy_40, "pre-NU6.3", Amount::zero()),
        (narrow.clone(), "zero NSM reserve", Amount::zero()),
        (wide.clone(), "non-zero NSM reserve", nsm_reserve),
    ] {
        let block_info = <BlockInfo as FromDisk>::from_bytes(&bytes);

        assert_eq!(
            block_info.size(),
            size,
            "the block size must be read from the right offset in a {layout} record",
        );

        let mut expected_pools = ValueBalance::zero();
        expected_pools.set_nsm_reserve_amount(expected_nsm_reserve);
        assert_eq!(
            *block_info.value_pools(),
            expected_pools,
            "the pools of a zeroed {layout} record must be zero apart from its NSM reserve",
        );
    }

    // The writer produces the narrow layout while the reserve is zero, and appends the reserve
    // after the block size once it is non-zero, so the size stays at its v28.0 offset.
    assert_eq!(
        BlockInfo::new(ValueBalance::zero(), size).as_bytes(),
        narrow,
        "a zero reserve must be written with the v28.0 layout",
    );

    let mut value_pools = ValueBalance::zero();
    value_pools.set_nsm_reserve_amount(nsm_reserve);
    assert_eq!(
        BlockInfo::new(value_pools, size).as_bytes(),
        wide,
        "a non-zero reserve must be appended after the block size",
    );
}
