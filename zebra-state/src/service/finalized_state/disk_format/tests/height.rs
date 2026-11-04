//! Canonical variable-width height and composite-codec regression tests.

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::Height,
    orchard,
    subtree::NoteCommitmentSubtreeData,
};

use crate::service::finalized_state::{
    disk_format::{
        block::{height_from_bytes_prefix, TransactionLocation},
        transparent::{
            AddressBalanceLocation, AddressLocation, AddressTransaction, AddressUnspentOutput,
            OutputLocation,
        },
        FromDisk, IntoDisk,
    },
    TransactionIndex,
};

const BOUNDARIES: &[(u32, &str)] = &[
    (0, "00"),
    (1, "0101"),
    (0xff, "01ff"),
    (0x100, "020100"),
    (0xffff, "02ffff"),
    (0x1_0000, "03010000"),
    (0xff_ffff, "03ffffff"),
    (0x100_0000, "0401000000"),
    (Height::MAX.0, "047fffffff"),
    (u32::MAX, "04ffffffff"),
];

#[test]
fn height_vectors_cover_zero_width_boundaries_and_u32() {
    let _guard = zebra_test::init();

    for &(value, expected_hex) in BOUNDARIES {
        let height = Height(value);
        let bytes = height.as_bytes();
        assert_eq!(hex::encode(&bytes), expected_hex, "height {value}");
        assert_eq!(Height::from_bytes(&bytes), height, "height {value}");
        assert_eq!(Height::from_bytes(&bytes).as_bytes(), bytes);
    }
}

#[test]
fn height_and_composite_keys_preserve_numeric_order() {
    let _guard = zebra_test::init();

    let mut heights: Vec<_> = BOUNDARIES
        .iter()
        .map(|(height, _)| Height(*height))
        .collect();
    heights.reverse();
    heights.sort_by_key(|height| height.as_bytes());
    let mut numeric: Vec<_> = BOUNDARIES
        .iter()
        .map(|(height, _)| Height(*height))
        .collect();
    numeric.sort();
    assert_eq!(heights, numeric);

    let mut locations: Vec<_> = BOUNDARIES
        .iter()
        .flat_map(|(height, _)| {
            [0, 1, u16::MAX].map(|index| TransactionLocation::from_index(Height(*height), index))
        })
        .collect();
    let mut numeric: Vec<_> = locations
        .iter()
        .map(|location| (location.height, location.index))
        .collect();
    numeric.sort();
    locations.reverse();
    locations.sort_by_key(|location| location.as_bytes());
    let ordered: Vec<_> = locations
        .iter()
        .map(|location| (location.height, location.index))
        .collect();
    assert_eq!(ordered, numeric);
}

#[test]
fn composite_locations_roundtrip_at_every_height_width() {
    let _guard = zebra_test::init();

    for &(height, _) in BOUNDARIES {
        let height = Height(height);
        let transaction =
            TransactionLocation::from_parts(height, TransactionIndex::from_index(0x1234));
        let transaction_bytes = transaction.as_bytes();
        assert_eq!(
            TransactionLocation::from_bytes(&transaction_bytes),
            transaction
        );
        assert_eq!(
            &transaction_bytes[transaction_bytes.len() - 2..],
            &[0x12, 0x34]
        );

        let output = OutputLocation::from_output_index(transaction, 0x01_0203);
        let output_bytes = output.as_bytes();
        assert_eq!(OutputLocation::from_bytes(&output_bytes), output);
        assert_eq!(&output_bytes[output_bytes.len() - 3..], &[1, 2, 3]);

        let other_transaction =
            TransactionLocation::from_index(Height(height.0.wrapping_add(0x0101_0101)), 0xabcd);
        let address_transaction = AddressTransaction::new(output, other_transaction);
        assert_eq!(
            AddressTransaction::from_bytes(address_transaction.as_bytes()),
            address_transaction
        );

        let other_output = OutputLocation::from_output_index(other_transaction, 0x0a_0b0c);
        let address_output = AddressUnspentOutput::new(output, other_output);
        assert_eq!(
            AddressUnspentOutput::from_bytes(address_output.as_bytes()),
            address_output
        );
    }
}

#[test]
fn balance_and_subtree_values_keep_unrelated_fields_unchanged() {
    let _guard = zebra_test::init();

    let location = AddressLocation::from_usize(Height(0x100), 0x1234, 0x01_0203);
    let mut balance = AddressBalanceLocation::new(location);
    *balance.balance_mut() = Amount::<NonNegative>::try_from(42).unwrap();
    *balance.received_mut() = 0x0102_0304_0506_0708;
    let bytes = balance.as_bytes();
    assert_eq!(&bytes[..8], &42_i64.to_le_bytes());
    assert_eq!(
        &bytes[bytes.len() - 8..],
        &0x0102_0304_0506_0708_u64.to_le_bytes()
    );
    assert_eq!(AddressBalanceLocation::from_bytes(&bytes), balance);

    let mut legacy_without_received = bytes[..bytes.len() - 8].to_vec();
    *balance.received_mut() = 0;
    assert_eq!(
        AddressBalanceLocation::from_bytes(&legacy_without_received),
        balance
    );
    legacy_without_received.push(0);
    assert!(std::panic::catch_unwind(|| {
        AddressBalanceLocation::from_bytes(&legacy_without_received)
    })
    .is_err());

    let subtree =
        NoteCommitmentSubtreeData::new(Height(0x100), orchard::tree::Node::from_bytes([7; 32]));
    let bytes = subtree.as_bytes();
    assert_eq!(&bytes[..3], &[2, 1, 0]);
    assert_eq!(&bytes[3..], &[7; 32]);
    assert_eq!(NoteCommitmentSubtreeData::from_bytes(&bytes), subtree);
}

#[test]
fn malformed_heights_and_composites_are_rejected() {
    let _guard = zebra_test::init();

    for bytes in [
        vec![],
        vec![1],
        vec![2, 1],
        vec![5, 1, 2, 3, 4, 5],
        vec![1, 0],
        vec![2, 0, 1],
    ] {
        assert!(height_from_bytes_prefix(&bytes).is_none(), "{bytes:?}");
        assert!(std::panic::catch_unwind(|| Height::from_bytes(&bytes)).is_err());
    }
    for bytes in [vec![0, 0], vec![1, 1, 0], vec![3, 1, 2, 3, 4]] {
        assert!(std::panic::catch_unwind(|| Height::from_bytes(&bytes)).is_err());
    }

    for bytes in [
        vec![],
        vec![1, 1, 0],
        vec![1, 1, 0, 0, 0],
        vec![1, 0, 0, 0],
        vec![0, 0, 1, 0, 0],
    ] {
        assert!(std::panic::catch_unwind(|| TransactionLocation::from_bytes(&bytes)).is_err());
        let optional = <Option<TransactionLocation>>::from_bytes(&bytes);
        assert_eq!(optional, None, "{bytes:?}");
    }

    for bytes in [
        vec![1, 1, 0, 0, 1],
        vec![1, 1, 0, 0, 1, 2, 3, 0],
        vec![2, 0, 1, 0, 0, 1, 2, 3],
    ] {
        assert!(std::panic::catch_unwind(|| OutputLocation::from_bytes(&bytes)).is_err());
    }

    let valid = AddressTransaction::new(
        OutputLocation::from_usize(Height(1), 2, 3),
        TransactionLocation::from_index(Height(0x100), 4),
    )
    .as_bytes();
    for truncated in [0, 1, valid.len() - 1] {
        assert!(std::panic::catch_unwind(|| {
            AddressTransaction::from_bytes(&valid[..truncated])
        })
        .is_err());
    }
    let mut trailing = valid.clone();
    trailing.push(0);
    assert!(std::panic::catch_unwind(|| AddressTransaction::from_bytes(&trailing)).is_err());

    let valid = AddressUnspentOutput::new(
        OutputLocation::from_usize(Height(1), 2, 3),
        OutputLocation::from_usize(Height(0x100), 4, 5),
    )
    .as_bytes();
    for truncated in [0, 1, valid.len() - 1] {
        assert!(std::panic::catch_unwind(|| {
            AddressUnspentOutput::from_bytes(&valid[..truncated])
        })
        .is_err());
    }
    let mut trailing = valid;
    trailing.push(0);
    assert!(std::panic::catch_unwind(|| AddressUnspentOutput::from_bytes(&trailing)).is_err());
}
