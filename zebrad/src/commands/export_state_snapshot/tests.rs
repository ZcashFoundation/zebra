//! Tests for the `export-state-snapshot` subcommand.

use zebra_chain::{
    block,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};

use crate::config::ZebradConfig;

use super::{commitment_toml, ExportStateSnapshotCmd};

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

/// The dedicated commitment output is a complete TOML document, with no log lines to scrape out.
#[test]
fn commitment_output_file_is_machine_readable_toml() {
    let _init_guard = zebra_test::init();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("commitment.toml");
    let command = ExportStateSnapshotCmd {
        commitment_out: Some(path.clone()),
        ..ExportStateSnapshotCmd::default()
    };
    let commitment = StateSnapshotCommitment {
        height: block::Height(42),
        block_hash: block::Hash([0xCD; 32]),
        manifest: ObjectHash([0xEF; 32]),
    };

    command.output_commitment(&commitment).unwrap();

    let output = std::fs::read_to_string(&path).unwrap();
    assert!(output.starts_with("# Add this table"), "{output}");
    assert!(output.contains("[sync.state_snapshot]"), "{output}");
    let config: ZebradConfig = toml::from_str(&output).expect("the whole file is valid TOML");
    assert_eq!(config.sync.state_snapshot, Some(commitment));

    let error = command
        .output_commitment(&commitment)
        .and_then(|()| super::write_commitment(dir.path(), "not a file"))
        .expect_err("writing over a directory fails");
    assert!(error.to_string().contains("could not write"), "{error}");
}
