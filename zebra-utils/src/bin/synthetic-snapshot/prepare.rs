//! Prepare A transactionally; publish readiness only after the production round trip.

use std::{collections::HashMap, fs, path::Path};

use zebra_chain::{block, block::Height};
use zebra_state::{
    mock::{build, prepare_header_context},
    state_snapshot::{
        compare::compare_states, export::export_state_snapshot, format::Manifest,
        import::import_state_snapshot, store::ArtifactStore,
    },
    BoxError, Config, MAX_BLOCK_REORG_HEIGHT,
};

use super::{
    config::{network, sync_directory, Resolved, TrialConfig},
    verify::expected_snapshot_base,
};

pub(crate) fn prepare(config_path: &Path) -> Result<(), BoxError> {
    let mut trial: TrialConfig = serde_json::from_slice(&fs::read(config_path)?)?;
    let checkpoint_height = trial.validate()?;

    let target = trial.state_config(config_path).cache_dir;
    if target.exists() {
        return Err(format!(
            "{} already exists; choose a new state directory (preparation never overwrites state)",
            target.display()
        )
        .into());
    }
    let parent = target
        .parent()
        .ok_or("state must have a parent directory")?;
    fs::create_dir_all(parent)?;
    let workspace = tempfile::Builder::new()
        .prefix(".synthetic-snapshot-")
        .tempdir_in(parent)?;
    let source = Config {
        cache_dir: workspace.path().join("source"),
        ..Config::default()
    };
    let genesis = block::genesis::regtest_genesis_block().hash();
    let initial_network = network(vec![(Height(0), genesis)]);
    eprintln!("prepare: building state at {}", trial.height.0);
    let tip_hash = build(source.cache_dir.clone(), &initial_network, trial.height)
        .map_err(|error| format!("build: {error}"))?;
    let expected_tip = (trial.height, tip_hash);

    let headers = prepare_header_context(&source, &initial_network, trial.tail)
        .map_err(|error| format!("header context: {error}"))?;
    let checkpoint_hash = headers.last().ok_or("header context is empty")?.hash();
    let checkpoints = vec![
        (Height(0), genesis),
        expected_tip,
        (checkpoint_height, checkpoint_hash),
    ];
    let network = network(checkpoints.clone());
    zebra_consensus::header::verify_headers_blocking(
        &network,
        Height(1),
        &headers,
        chrono::Utc::now(),
    )
    .map_err(|error| format!("header verification: {error}"))?;

    eprintln!("prepare: exporting production snapshot");
    let store = ArtifactStore::new(&source, &network);
    let commitment = export_state_snapshot(&source, &network, trial.height, &store)
        .map_err(|error| format!("export: {error}"))?;
    let manifest = Manifest::decode(&fs::read(store.object_path(commitment.manifest))?, &network)?;
    let pieces = manifest
        .pieces()
        .map(|(_, _, piece)| (piece.hash, store.object_path(piece.hash)))
        .collect::<HashMap<_, _>>();

    let imported = Config {
        cache_dir: workspace.path().join("gate"),
        prune_blocks_older_than: Some(MAX_BLOCK_REORG_HEIGHT + 1),
        ..Config::default()
    };
    let import_workspace = workspace.path().join("import");
    fs::create_dir(&import_workspace)?;
    eprintln!("prepare: importing production snapshot");
    let expected_base = expected_snapshot_base(commitment);
    // A serves header context, not tail bodies. Production bootstrap records the same
    // deferred successor check when no peer can supply H+1; the finalized tip remains H.
    import_state_snapshot(
        &import_workspace,
        &manifest,
        commitment,
        expected_base.deferred_checks,
        &headers[usize::try_from(trial.height.0)?..],
        &network,
        &imported,
        &pieces,
    )
    .map_err(|error| format!("import: {error}"))?;
    eprintln!("prepare: comparing independently opened state rows");
    compare_states(&source, &imported, &network, expected_base)
        .map_err(|error| format!("comparison: {error}"))?;

    // Remove the gate before publishing A; nothing but the source and snapshot store survives.
    fs::remove_dir_all(&imported.cache_dir)?;
    fs::remove_dir_all(&import_workspace)?;
    fs::rename(&source.cache_dir, &target)?;
    sync_directory(parent)?;
    trial.resolved = Some(Resolved {
        preparation_input: trial.preparation_input()?,
        commitment,
        checkpoints,
        expected_tip,
    });
    if let Err(error) = trial.save(config_path) {
        fs::remove_dir_all(&target)?;
        return Err(error);
    }
    println!(
        "prepared {} at height {} (export/import/row comparison passed)",
        target.display(),
        trial.height.0
    );
    Ok(())
}
