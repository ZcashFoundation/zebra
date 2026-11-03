//! Tests for the `export-state-snapshot` subcommand.

use zebra_chain::{
    block,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};

use crate::config::ZebradConfig;

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

    // Pasted into an empty config, it is the only change from the defaults.
    let config: ZebradConfig = toml::from_str(&printed).expect("the printed table is valid config");
    assert_eq!(config.sync.state_snapshot, Some(commitment));
    let mut expected = ZebradConfig::default();
    expected.sync.state_snapshot = Some(commitment);
    assert_eq!(config, expected);

    // And after the keys of an existing [sync] section.
    let config = format!("[sync]\nstate_snapshot_sync = true\n\n{printed}");
    let config: ZebradConfig = toml::from_str(&config).expect("valid config");
    assert!(config.sync.state_snapshot_sync);
    assert_eq!(config.sync.state_snapshot, Some(commitment));
}
