//! Tests for the `export-state-snapshot` subcommand.

use zebra_chain::{
    block,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};

use super::commitment_toml;

/// The printed commitment is a config table that configures the node to
/// bootstrap from the snapshot, with the lines the bootstrap tests match.
#[test]
fn printed_commitment_configures_the_sync_section() {
    let _init_guard = zebra_test::init();

    let commitment = StateSnapshotCommitment {
        height: block::Height(3_522_060),
        block_hash: block::Hash(std::array::from_fn(|index| {
            u8::try_from(index).expect("32 indexes fit in a byte")
        })),
        manifest: ObjectHash([0xAB; 32]),
    };
    let printed = commitment_toml(&commitment);

    let lines: Vec<&str> = printed.lines().collect();
    assert!(lines.contains(&"[sync.state_snapshot]"), "{printed}");
    assert!(lines.contains(&"height = 3522060"), "{printed}");
    assert!(
        lines.contains(&format!("hash = \"{}\"", commitment.block_hash).as_str()),
        "{printed}",
    );
    assert!(
        lines.contains(&format!("manifest = \"{}\"", commitment.manifest).as_str()),
        "{printed}",
    );

    let parsed: toml::Value = toml::from_str(&printed).expect("the printed table is valid TOML");
    let snapshot = parsed
        .get("sync")
        .and_then(|sync| sync.get("state_snapshot"))
        .expect("the printed table is in the sync section");
    assert_eq!(
        snapshot.get("height").and_then(toml::Value::as_integer),
        Some(3_522_060)
    );

    let after_sync_keys = format!("[sync]\nstate_snapshot_sync = true\n\n{printed}");
    toml::from_str::<toml::Value>(&after_sync_keys).expect("valid TOML after existing sync keys");
}
