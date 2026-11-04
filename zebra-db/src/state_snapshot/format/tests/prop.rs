//! Randomised round trips of manifests and pieces.

use std::{collections::BTreeMap, io::Cursor};

use proptest::{collection::vec, prelude::*};

use zebra_chain::{block, parameters::Network, state_snapshot::ObjectHash};

use crate::state_snapshot::format::{
    ComponentId, ComponentManifest, Manifest, PieceManifest, PieceReader, PieceRecord, PieceWriter,
    INDEX_MODE_DEFAULT, MAX_PIECE_BYTES,
};

/// The pieces of one component: none, or a few with any valid size.
fn pieces() -> impl Strategy<Value = Vec<PieceManifest>> {
    // `MAX_PIECE_BYTES` is 16 MiB, which fits in a `u32`.
    let size = 1..=MAX_PIECE_BYTES as u32;
    vec(
        (size, any::<ObjectHash>()).prop_map(|(size, hash)| PieceManifest { size, hash }),
        0..4,
    )
}

/// A valid manifest for Mainnet, or for Regtest, which carries a parameters
/// hash.
fn manifest() -> impl Strategy<Value = (Network, Manifest)> {
    (
        any::<bool>(),
        0..=block::Height::MAX.0,
        any::<u32>(),
        any::<[u8; 32]>(),
        any::<u16>(),
        any::<[u8; 32]>(),
        vec((pieces(), 1..=u64::MAX), ComponentId::ALL.len()),
    )
        .prop_map(
            |(regtest, height, window, block_hash, db_format_major, parameters, components)| {
                let network = if regtest {
                    Network::new_regtest(Default::default())
                } else {
                    Network::Mainnet
                };
                let manifest = Manifest {
                    genesis_hash: network.genesis_hash(),
                    height: block::Height(height),
                    block_hash: block::Hash(block_hash),
                    window_start: block::Height(window % (height + 1)),
                    db_format_major,
                    index_mode: INDEX_MODE_DEFAULT,
                    parameters_hash: Manifest::needs_parameters_hash(&network)
                        .then_some(parameters),
                    components: ComponentId::ALL
                        .into_iter()
                        .zip(components)
                        .map(|(id, (pieces, entries))| ComponentManifest {
                            id,
                            entry_count: if pieces.is_empty() { 0 } else { entries },
                            pieces,
                        })
                        .collect(),
                };
                (network, manifest)
            },
        )
}

/// Records with distinct keys, in key order, with lengths that need every
/// CompactSize width up to 3 bytes.
fn records() -> impl Strategy<Value = Vec<(Vec<u8>, Vec<u8>)>> {
    let bytes = || prop_oneof![vec(any::<u8>(), 0..8), vec(any::<u8>(), 0xF0..0x110),];
    vec((bytes(), bytes()), 0..24).prop_map(|records| {
        records
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect()
    })
}

proptest! {
    /// Every valid manifest round trips, and decoding then encoding gives back
    /// the same bytes.
    #[test]
    fn manifests_round_trip((network, manifest) in manifest()) {
        let _init_guard = zebra_test::init();

        let bytes = manifest.encode().expect("a valid manifest");
        let decoded = Manifest::decode(&bytes, &network).expect("round trips");
        prop_assert_eq!(&decoded, &manifest);
        prop_assert_eq!(decoded.encode().expect("valid"), bytes);
    }

    /// Records written as a piece read back as the same records, and the piece
    /// is the concatenation of their encodings, so it depends only on them.
    #[test]
    fn pieces_round_trip(records in records()) {
        let _init_guard = zebra_test::init();

        let mut writer = PieceWriter::new(Vec::new());
        let mut expected = Vec::new();
        for (key, value) in &records {
            writer.push(key, value).expect("ordered records that fit");
            expected.extend(PieceRecord { key: key.clone(), value: value.clone() }
                .encode()
                .expect("fits"));
        }
        prop_assert_eq!(writer.records(), u64::try_from(records.len()).expect("a few records"));
        let bytes = writer.finish().expect("flushes");
        prop_assert_eq!(&bytes, &expected);

        let mut reader = PieceReader::new(Cursor::new(&bytes));
        let mut decoded = Vec::new();
        while let Some(PieceRecord { key, value }) = reader.next_record().expect("valid piece") {
            decoded.push((key, value));
        }
        prop_assert_eq!(decoded, records);
        prop_assert_eq!(reader.bytes_read(), bytes.len());
    }
}
