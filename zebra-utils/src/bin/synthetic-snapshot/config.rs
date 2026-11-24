//! One user-authored configuration, including the preparation gate's resolved values.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zebra_chain::{
    block::{self, Height},
    parameters::{
        checkpoint::list::CheckpointList,
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
    state_snapshot::StateSnapshotCommitment,
};
use zebra_state::{
    state_database_format_version_in_code,
    state_snapshot::{
        compare::validate_snapshot_source,
        format::{window_start, Manifest, MAX_MANIFEST_BYTES},
        store::ArtifactStore,
    },
    BoxError, Config,
};

/// Paths are relative to the config file, not the process working directory.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrialConfig {
    pub state: PathBuf,
    pub height: Height,
    pub tail: u32,
    pub trials: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<Resolved>,
}

/// Written only after production export, import, and independent row comparison succeed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Resolved {
    pub preparation_input: String,
    pub commitment: StateSnapshotCommitment,
    pub checkpoints: Vec<(Height, block::Hash)>,
    pub expected_tip: (Height, block::Hash),
}

/// An admitted preparation, checked against the source database and artifact store.
pub(crate) struct Prepared {
    pub config: TrialConfig,
    pub network: Network,
    pub source: zebra_state::Config,
}

impl Prepared {
    /// Loads a completed preparation without modifying its source state or artifacts.
    pub(crate) fn load(config_path: &Path) -> Result<Self, BoxError> {
        let config: TrialConfig = serde_json::from_slice(&fs::read(config_path)?)?;
        let tail_height = config.validate()?;
        let resolved = config
            .resolved
            .as_ref()
            .ok_or("configuration is not prepared: run prepare first")?;
        if resolved.preparation_input != config.preparation_input()? {
            return Err(
                "preparation inputs changed: prepare a new source before continuing".into(),
            );
        }
        let height = config.height;
        if resolved.expected_tip.0 != height
            || (resolved.commitment.height, resolved.commitment.block_hash) != resolved.expected_tip
        {
            return Err("resolved expected_tip and commitment must agree at height".into());
        }
        let genesis = block::genesis::regtest_genesis_block().hash();
        if resolved.checkpoints.len() != 3
            || resolved.checkpoints[0] != (Height(0), genesis)
            || resolved.checkpoints[1] != resolved.expected_tip
            || resolved.checkpoints[2].0 != tail_height
        {
            return Err(
                "resolved checkpoints must be exactly Regtest genesis, expected_tip at H, and H + tail"
                    .into(),
            );
        }
        // The network constructor expects valid checkpoints and can panic otherwise.
        CheckpointList::from_list(resolved.checkpoints.iter().copied())
            .map_err(|error| format!("invalid resolved checkpoints: {error}"))?;
        let network = network(resolved.checkpoints.clone());
        let source = config.state_config(config_path);
        validate_snapshot_source(
            &source,
            &network,
            resolved.expected_tip,
            &resolved.checkpoints,
        )
        .map_err(|error| {
            format!(
                "cannot read prepared source {}: {error}",
                source.cache_dir.display()
            )
        })?;

        let store = ArtifactStore::new(&source, &network);
        let manifest_hash = resolved.commitment.manifest;
        if store.is_rejected(manifest_hash) {
            return Err(format!("prepared manifest {manifest_hash} is marked rejected").into());
        }
        let size = store.size(manifest_hash)?.ok_or_else(|| {
            format!(
                "prepared manifest {manifest_hash} is missing from {}",
                store.root().display()
            )
        })?;
        if size > u64::try_from(MAX_MANIFEST_BYTES)? || !store.contains_verified(manifest_hash)? {
            return Err(
                format!("prepared manifest {manifest_hash} is oversized or corrupt").into(),
            );
        }
        let bytes = store.read_at(manifest_hash, 0, u32::try_from(size)?)?;
        let manifest = Manifest::decode(&bytes, &network)
            .map_err(|error| format!("invalid prepared manifest {manifest_hash}: {error}"))?;
        if manifest.genesis_hash != genesis
            || (manifest.height, manifest.block_hash) != resolved.expected_tip
            || manifest.parameters_hash != network.state_snapshot_parameters_hash()
            || manifest.window_start != window_start(height)
            || u64::from(manifest.db_format_major) != state_database_format_version_in_code().major
        {
            return Err(
                "prepared manifest does not match the resolved source, network, or database format"
                    .into(),
            );
        }
        let mut verified_pieces = HashMap::new();
        for (_, _, piece) in manifest.pieces() {
            if let Some(size) = verified_pieces.get(&piece.hash) {
                if *size != piece.size {
                    return Err(format!(
                        "prepared manifest gives piece {} conflicting sizes",
                        piece.hash
                    )
                    .into());
                }
                continue;
            }
            if store.size(piece.hash)? != Some(u64::from(piece.size))
                || !store.contains_verified(piece.hash)?
            {
                return Err(format!(
                    "prepared snapshot piece {} is missing, corrupt, or has the wrong size in {}",
                    piece.hash,
                    store.root().display(),
                )
                .into());
            }
            verified_pieces.insert(piece.hash, piece.size);
        }
        Ok(Self {
            config,
            network,
            source,
        })
    }
}

impl TrialConfig {
    pub(crate) fn validate(&self) -> Result<Height, BoxError> {
        if self.state.as_os_str().is_empty() {
            return Err("state must name a new preparation directory".into());
        }
        if self.trials == 0 {
            return Err("trials must be at least 1".into());
        }
        (self.height + i64::from(self.tail))
            .filter(|_| self.tail != 0)
            .ok_or_else(|| "tail must be positive and H + tail must be a valid block height".into())
    }

    pub(crate) fn state_config(&self, config_path: &Path) -> Config {
        Config {
            cache_dir: config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(&self.state),
            ..Config::default()
        }
    }

    /// Trial count does not affect prepared state, so changing it does not require rebuilding.
    pub(crate) fn preparation_input(&self) -> Result<String, BoxError> {
        // Bind the fixed mock-state fixture and committed-context recipe as well as user inputs.
        let bytes = serde_json::to_vec(&(3u32, &self.state, self.height, self.tail))?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    /// Atomically replaces the same config; no second fixture or readiness file is created.
    pub(crate) fn save(&self, path: &Path) -> Result<(), BoxError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        writeln!(file)?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        sync_directory(parent)?;
        Ok(())
    }
}

/// The mock's activation schedule is fixed; the actual chain supplies its checkpoints.
pub(crate) fn network(checkpoints: Vec<(Height, block::Hash)>) -> Network {
    Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            nu6_3: Some(1),
            ..Default::default()
        },
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(checkpoints)),
        ..Default::default()
    })
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), BoxError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
