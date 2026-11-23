//! Production-process proof for serving an unchanged prepared synthetic snapshot.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use tower::{Service, ServiceExt};
use zebra_chain::{
    block::{self, Height},
    parameters::{
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
    state_snapshot::StateSnapshotCommitment,
};
use zebra_network::{v2_bootstrap_clients, Request, Response};
use zebra_state::{
    constants::STATE_DATABASE_KIND,
    state_database_format_version_in_code,
    state_snapshot::{format::Manifest, store::ArtifactStore},
    BoxError, Config, ZebraDb,
};

use super::{node::SnapshotNode, prepare::prepare, verify::expected_snapshot_base};

const HEIGHT: u32 = 120;
const START_TIMEOUT: Duration = Duration::from_secs(60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Opens the real primary, rather than a secondary that could hide a leaked primary lock.
fn open_primary(config: &Config, network: &Network) -> Result<ZebraDb, BoxError> {
    Ok(ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        network,
        true,
        std::iter::empty::<String>(),
        false,
    )?)
}

/// A missing /proc entry also excludes an unreaped zombie on Linux. Never signal a saved PID.
fn assert_reaped(pid: u32) {
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new("/proc").join(pid.to_string()).exists(),
        "stopped source process {pid} still exists (possibly unreaped)"
    );
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "build ordinary zebrad and set ZEBRAD_BIN to its absolute path; prepares real RocksDB state"]
async fn prepared_source_serves_headers_and_every_snapshot_object() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();
    for tail in [1, 2] {
        serve_prepared_tail(tail).await?;
    }
    Ok(())
}

async fn serve_prepared_tail(tail: u32) -> Result<(), BoxError> {
    let zebrad = PathBuf::from(
        std::env::var_os("ZEBRAD_BIN")
            .ok_or("set ZEBRAD_BIN to the ordinary production zebrad executable")?,
    )
    .canonicalize()?;
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("trial.json");
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "state": "source",
            "height": HEIGHT,
            "tail": tail,
            "trials": 1
        }))?,
    )?;

    // Exercise real preparation, including the production export/import/comparison gate.
    let prepare_config = config_path.canonicalize()?;
    tokio::task::spawn_blocking(move || prepare(&prepare_config)).await??;

    let prepared: serde_json::Value = serde_json::from_slice(&fs::read(&config_path)?)?;
    let resolved = &prepared["resolved"];
    let checkpoints: Vec<(Height, block::Hash)> =
        serde_json::from_value(resolved["checkpoints"].clone())?;
    let expected_tip: (Height, block::Hash) =
        serde_json::from_value(resolved["expected_tip"].clone())?;
    let commitment: StateSnapshotCommitment =
        serde_json::from_value(resolved["commitment"].clone())?;
    assert_eq!(expected_tip.0, Height(HEIGHT));
    assert_eq!(
        checkpoints.last().map(|checkpoint| checkpoint.0),
        Some(Height(HEIGHT + tail))
    );
    let network = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            nu6_3: Some(1),
            ..Default::default()
        },
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(checkpoints)),
        ..Default::default()
    });
    let source = Config {
        cache_dir: workspace.path().join(
            prepared["state"]
                .as_str()
                .ok_or("prepared config must retain its state path")?,
        ),
        ..Config::default()
    };
    let primary = open_primary(&source, &network)?;
    assert_eq!(primary.tip(), Some(expected_tip));
    let expected_headers = (1..=HEIGHT + tail)
        .map(|height| {
            primary
                .any_header(Height(height))
                .map(|header| header.hash())
                .ok_or_else(|| format!("prepared header {height} is missing"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    drop(primary);
    let serving = Config {
        cache_dir: workspace.path().join("serving-state"),
        prune_blocks_older_than: Some(
            zebra_network::constants::NODE_NETWORK_LIMITED_BLOCKS
                .max(zebra_state::MAX_BLOCK_REORG_HEIGHT + 1),
        ),
        ..Config::default()
    };
    let serving_height = zebra_state::mock::prepare_serving_state(
        &source,
        &network,
        tail,
        &serving,
        commitment,
        zebra_network::constants::NODE_NETWORK_LIMITED_BLOCKS + zebra_state::MAX_BLOCK_REORG_HEIGHT,
    )?;
    let primary = open_primary(&serving, &network)?;
    assert_eq!(primary.tip().map(|tip| tip.0), Some(serving_height));
    assert!(matches!(
        primary.block(Height(HEIGHT + 1).into()),
        Err(zebra_state::BlockReadError::BlockBodyUnavailable { height })
            if height == Height(HEIGHT + 1)
    ));

    // On Linux retain the failed child's actual PID across exec, so even a zombie
    // cannot be mistaken for successful cleanup merely because its lock is gone.
    #[cfg(target_os = "linux")]
    let (failed_executable, failed_pid_path) = {
        use std::os::unix::fs::PermissionsExt;

        let wrapper = workspace.path().join("record-source-pid");
        let pid_path = workspace.path().join("failed-source.pid");
        let quote = |path: &Path| -> Result<String, BoxError> {
            Ok(format!(
                "'{}'",
                path.to_str()
                    .ok_or("test executable paths must be UTF-8")?
                    .replace('\'', "'\\''")
            ))
        };
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" > {}\nexec {} \"$@\"\n",
                quote(&pid_path)?,
                quote(&zebrad)?,
            ),
        )?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
        (wrapper, pid_path)
    };
    #[cfg(not(target_os = "linux"))]
    let failed_executable = zebrad.clone();

    // Unlike an invalid executable, a locked primary exercises a real zebrad child:
    // it cannot become ready while this process owns the database. Releasing our
    // handle must permit an immediate successful start on the very same source.
    let failed = SnapshotNode::start(
        &failed_executable,
        &network,
        &serving,
        &workspace.path().join("locked-start"),
        serving_height.0,
        2,
        START_TIMEOUT,
    )
    .await;
    assert!(failed.is_err(), "a second primary must not become ready");
    #[cfg(target_os = "linux")]
    assert_reaped(fs::read_to_string(failed_pid_path)?.trim().parse()?);
    drop(primary);

    let mut node = SnapshotNode::start(
        &zebrad,
        &network,
        &serving,
        &workspace.path().join("serving"),
        serving_height.0,
        2,
        START_TIMEOUT,
    )
    .await?;
    let pid = node.id();
    let store = ArtifactStore::new(&source, &network);
    let manifest_bytes = fs::read(store.object_path(commitment.manifest))?;
    let manifest = Manifest::decode(&manifest_bytes, &network)?;
    let client_config = zebra_network::Config {
        network: network.clone(),
        ..Default::default()
    };
    // The production dialer bounds all attempts and closes partially created endpoints on error.
    let mut bootstrap = v2_bootstrap_clients(
        &client_config,
        &[node.quic_addr.into()],
        "/synthetic-snapshot-serving-test/".to_owned(),
    )
    .await?;

    // Keep assertions as errors until close() has run, including on timeout or a bad response.
    let fetched = tokio::time::timeout(FETCH_TIMEOUT, async {
        if bootstrap.clients.len() != 1 {
            return Err("prepared source did not accept the production bootstrap client".into());
        }
        let client = &mut bootstrap.clients[0].1;
        for (locator, expected) in [
            (
                block::genesis::regtest_genesis_block().hash(),
                expected_headers.as_slice(),
            ),
            (
                expected_tip.1,
                &expected_headers[usize::try_from(HEIGHT)?..],
            ),
        ] {
            let response = client
                .ready()
                .await?
                .call(Request::FindHeaders {
                    known_blocks: vec![locator],
                    stop: expected_headers.last().copied(),
                })
                .await?;
            let Response::BlockHeaders(headers) = response else {
                return Err(format!("expected checkpoint headers, got {response}").into());
            };
            let last = headers.last().map(|header| header.header.hash());
            if headers.len() != expected.len()
                || headers
                    .iter()
                    .zip(expected)
                    .any(|(header, expected)| header.header.hash() != *expected)
            {
                return Err(format!(
                    "locator {locator}: served {} headers ending {last:?}; expected {} ending {:?}",
                    headers.len(),
                    expected.len(),
                    expected.last(),
                )
                .into());
            }
        }

        let successor = expected_headers[usize::try_from(HEIGHT)?];
        let response = client
            .ready()
            .await?
            .call(Request::BlocksByHash([successor].into_iter().collect()))
            .await;
        match response {
            // SharedPeerError exposes the wrapped TracedError only through inner_debug.
            Err(error) if error.inner_debug().starts_with("NotFoundResponse(") => {}
            Err(error) => return Err(error.into()),
            Ok(response) => {
                return Err(format!(
                    "serving view must refuse H+1, not advance the imported tip: {response}"
                )
                .into())
            }
        }

        // Use the production manifest only to enumerate objects. The assertion is a byte
        // comparison against the prepared store, not a parallel decoder or validator.
        for hash in std::iter::once(commitment.manifest)
            .chain(manifest.pieces().map(|(_, _, piece)| piece.hash))
        {
            let expected = fs::read(store.object_path(hash))?;
            let mut offset = 0;
            while offset < expected.len() {
                let length = u32::try_from((expected.len() - offset).min(32 * 1024 * 1024))?;
                let response = client
                    .ready()
                    .await?
                    .call(Request::Object {
                        hash,
                        offset: u64::try_from(offset)?,
                        length,
                    })
                    .await?;
                let Response::Object { size, data } = response else {
                    return Err(format!("expected prepared object {hash}, got {response}").into());
                };
                let end = offset
                    .checked_add(data.len())
                    .ok_or("object offset overflow")?;
                if size != u64::try_from(expected.len())?
                    || data.is_empty()
                    || expected.get(offset..end) != Some(data.as_slice())
                {
                    return Err(format!("served object {hash} differs at offset {offset}").into());
                }
                offset = end;
            }
        }
        Ok::<(), BoxError>(())
    })
    .await;
    bootstrap.close().await;
    fetched??;
    for trial in 1..=2 {
        let trial_workspace = workspace.path().join(format!("trial-{trial}"));
        let mut imported = SnapshotNode::start_bootstrap(
            &zebrad,
            &network,
            &trial_workspace,
            commitment,
            node.quic_addr,
            FETCH_TIMEOUT,
        )
        .await
        .map_err(|error| {
            format!(
                "{error}; B bootstrap logs: stdout={:?}; stderr={:?}",
                fs::read_to_string(trial_workspace.join("bootstrap.stdout.log")),
                fs::read_to_string(trial_workspace.join("bootstrap.stderr.log"))
            )
        })?;
        let imported_pid = imported.id();
        let checked = tokio::time::timeout(START_TIMEOUT, async {
            imported
                .check_liveness(&mut node, expected_tip, Height(HEIGHT), START_TIMEOUT)
                .await?;
            // A successful bootstrap is not enough: the shared liveness check must
            // refuse a retained floor other than the prepared snapshot height.
            assert!(imported
                .check_liveness(&mut node, expected_tip, Height(HEIGHT - 1), START_TIMEOUT)
                .await
                .is_err());
            Ok::<(), BoxError>(())
        })
        .await;
        if cfg!(unix) && trial == 2 {
            // An unexpected natural exit must not be accepted as successful owned shutdown.
            imported.request_stop(START_TIMEOUT).await?;
            tokio::time::timeout(START_TIMEOUT, async {
                while imported.require_running().is_ok() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await?;
            assert!(imported.stop().is_err());
        } else {
            imported.stop()?;
        }
        assert_reaped(imported_pid);
        checked??;
        let expected = source.clone();
        let imported = Config {
            cache_dir: trial_workspace.join("cache"),
            prune_blocks_older_than: Some(zebra_state::MAX_BLOCK_REORG_HEIGHT + 1),
            ..Default::default()
        };
        let comparison_network = network.clone();
        tokio::task::spawn_blocking(move || {
            zebra_state::state_snapshot::compare::compare_states(
                &expected,
                &imported,
                &comparison_network,
                expected_snapshot_base(commitment),
            )
        })
        .await??;
        node.require_running()?;
    }
    let stopped = node.stop();
    stopped?;
    assert_reaped(pid);

    // A real primary, not a secondary, proves the serving child released its database.
    let serving_db = open_primary(&serving, &network)?;
    assert_eq!(serving_db.tip().map(|tip| tip.0), Some(serving_height));
    drop(serving_db);
    // The independent comparison source and its prepared header context remain unchanged.
    let reopened = open_primary(&source, &network)?;
    assert_eq!(reopened.tip(), Some(expected_tip));
    for (height, hash) in (1..=HEIGHT + tail).zip(expected_headers) {
        assert_eq!(
            reopened
                .any_header(Height(height))
                .map(|header| header.hash()),
            Some(hash)
        );
    }
    drop(reopened);
    assert_eq!(
        zebra_state::state_snapshot::export::export_state_snapshot(
            &source,
            &network,
            Height(HEIGHT),
            &store,
        )?,
        commitment,
        "serving must not change the comparison source's snapshot-selected state",
    );
    Ok(())
}

/// A serving snapshot above the retained window still needs the earlier header chain.
#[test]
#[ignore = "prepares real RocksDB state; run with the production-serving checks"]
fn serving_view_preserves_headers_below_snapshot_window() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();
    let workspace = tempfile::tempdir()?;
    let height = zebra_network::constants::NODE_NETWORK_LIMITED_BLOCKS + 26;
    let config_path = workspace.path().join("trial.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&serde_json::json!({
            "state": "source",
            "height": height,
            "tail": 2,
            "trials": 1
        }))?,
    )?;
    prepare(&config_path.canonicalize()?)?;
    let prepared: serde_json::Value = serde_json::from_slice(&fs::read(config_path)?)?;
    let network = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            nu6_3: Some(1),
            ..Default::default()
        },
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(
            serde_json::from_value(prepared["resolved"]["checkpoints"].clone())?,
        )),
        ..Default::default()
    });
    let commitment = serde_json::from_value(prepared["resolved"]["commitment"].clone())?;
    let source = Config {
        cache_dir: workspace.path().join("source"),
        ..Config::default()
    };
    let serving = Config {
        cache_dir: workspace.path().join("serving"),
        prune_blocks_older_than: Some(zebra_state::MAX_BLOCK_REORG_HEIGHT + 1),
        ..Config::default()
    };
    let tip =
        zebra_state::mock::prepare_serving_state(&source, &network, 2, &serving, commitment, 0)?;
    let window_start = zebra_state::state_snapshot::format::window_start(tip);
    assert!(window_start > Height(1));
    let original = open_primary(&source, &network)?;
    let served = open_primary(&serving, &network)?;
    assert_eq!(
        original.tip(),
        Some((Height(height), commitment.block_hash))
    );
    for height in 1..window_start.0 {
        let expected = original
            .any_header(Height(height))
            .ok_or("prepared source header is missing")?;
        assert_eq!(
            served
                .any_header(Height(height))
                .map(|header| header.hash()),
            Some(expected.hash()),
            "serving view lost header {height} below the snapshot window"
        );
    }
    Ok(())
}
