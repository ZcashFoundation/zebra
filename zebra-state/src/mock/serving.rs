//! Disposable production-serving view; the prepared comparison database stays at H.

use std::{
    collections::{HashMap, VecDeque},
    fs,
    sync::Arc,
};

use zebra_chain::{
    amount::Amount,
    block::{self, Block, Height},
    parameters::{
        testnet::{ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
    state_snapshot::StateSnapshotCommitment,
    transparent,
};

use super::blocks::{child, commit};
use crate::{
    constants::STATE_DATABASE_KIND,
    service::{
        check::{check_header_context, difficulty::MAX_POW_ADJUSTMENT_BLOCK_SPAN},
        finalized_state::{FinalizedState, MAX_ON_DISK_HEIGHT},
        header_write::{commit_headers, header_lookahead_limit, MAX_COMMIT_HEADERS},
    },
    state_database_format_version_in_code,
    state_snapshot::{
        export::export_state_snapshot,
        format::{window_start, Manifest},
        import::{carry_header_chain, import_state_snapshot},
        store::ArtifactStore,
        SnapshotBase,
    },
    BoxError, Config, DiskWriteBatch,
};

/// Derives a disposable serving database from the prepared comparison source.
///
/// Ordinary FindHeaders ends at the committed tip, and the peer set requires block service
/// for header requests. First export/import an independent checkpoint at H + max(tail, 2),
/// so H+1 stays unavailable. Then append `retained_blocks` inert context bodies to let the
/// production node advertise limited block service. Callers supply the production limited
/// window plus its finalization margin. B still cannot advance past the missing H+1.
/// This is fixture construction, not an alternative peering or validation path.
///
/// `destination` must be fresh and pruned. Only the original prepared snapshot's
/// objects are published for serving; the context snapshot is temporary.
/// Returns the serving tip height, which is not the comparison/installed tip.
/// The network must use the synthetic trial's standard Regtest parameters, with
/// only upgrade heights and checkpoints configured.
pub fn prepare_serving_state(
    source: &Config,
    network: &Network,
    tail: u32,
    destination: &Config,
    commitment: StateSnapshotCommitment,
    retained_blocks: u32,
) -> Result<Height, BoxError> {
    if tail == 0 {
        return Err("the serving context requires a positive tail".into());
    }
    if destination.ephemeral || destination.prune_blocks_older_than.is_none() {
        return Err("the disposable serving view must be persistent and pruned".into());
    }
    if destination.cache_dir.exists() {
        return Err("the disposable serving directory must not already exist".into());
    }
    let parent = destination
        .cache_dir
        .parent()
        .ok_or("serving directory has no parent")?;
    fs::create_dir_all(parent)?;
    let workspace = tempfile::tempdir_in(parent)?;
    let context_config = Config {
        cache_dir: workspace.path().join("context"),
        ..source.clone()
    };
    let HeaderContext {
        state: context,
        headers,
        source_height,
    } = clone_with_tail(source, network, tail.max(2), &context_config)?;
    if source_height != commitment.height
        || headers[usize::try_from(source_height.0 - 1)?].hash() != commitment.block_hash
    {
        return Err("prepared commitment does not match the comparison source".into());
    }
    let checkpoint_height = (source_height + i64::from(tail)).ok_or("tail height overflow")?;
    let checkpoint = network.checkpoint_list().hash(checkpoint_height);
    if checkpoint != Some(headers[usize::try_from(checkpoint_height.0 - 1)?].hash()) {
        return Err("prepared header context is stale: prepare a new source".into());
    }
    let serving_height = context.db.tip().ok_or("serving context has no tip")?.0;
    let final_height = (serving_height + i64::from(retained_blocks))
        .filter(|height| *height <= MAX_ON_DISK_HEIGHT)
        .ok_or("serving block window exceeds the maximum on-disk height")?;
    let context_path = context.db.path().to_owned();
    // The production importer requires a checkpoint above its own snapshot.
    // Derive a real extra header solely for this disposable fixture's import;
    // the trial network and its H+tail checkpoint are not changed.
    let tip_block = context
        .db
        .block(serving_height.into())?
        .ok_or("serving context tip body is missing")?;
    let successor = context_child(network, &context, &tip_block);
    let mut checkpoints: Vec<_> = network.checkpoint_list().iter_cloned().collect();
    checkpoints.push((
        (serving_height + 1).ok_or("context checkpoint height overflow")?,
        successor.hash(),
    ));
    let import_network = Network::new_regtest(RegtestParameters {
        activation_heights: network.activation_list().into(),
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(checkpoints)),
        ..Default::default()
    });
    // The importer carries a headers-first chain, not headers embedded in block rows.
    // Materialize only the prefix omitted by this snapshot's window, using the headers
    // already checked by clone_with_tail. All writes stay in the disposable clone.
    let prefix_len = usize::try_from(window_start(serving_height).0 - 1)?;
    for (index, chunk) in headers[..prefix_len].chunks(MAX_COMMIT_HEADERS).enumerate() {
        let mut batch = DiskWriteBatch::new();
        if index == 0 {
            batch.prepare_header_chain_truncate(&context.db, Height(1));
        }
        let first_height = Height(u32::try_from(index * MAX_COMMIT_HEADERS + 1)?);
        batch.prepare_header_chain_append(&context.db, first_height, chunk);
        context.db.write_batch(batch)?;
    }
    drop(context);
    let store = ArtifactStore::new(&context_config, network);
    let serving_commitment =
        export_state_snapshot(&context_config, network, serving_height, &store)?;
    let manifest = Manifest::decode(
        &fs::read(store.object_path(serving_commitment.manifest))?,
        network,
    )?;
    let pieces = manifest
        .pieces()
        .map(|(_, _, piece)| (piece.hash, store.object_path(piece.hash)))
        .collect::<HashMap<_, _>>();
    let import_root = workspace.path().join("import");
    fs::create_dir(&import_root)?;
    import_state_snapshot(
        &import_root,
        &manifest,
        serving_commitment,
        SnapshotBase::DEFERRED_BLOCK_HASH | SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS,
        &[],
        &import_network,
        destination,
        &pieces,
    )?;
    carry_header_chain(
        &context_path,
        &destination.db_path(
            STATE_DATABASE_KIND,
            state_database_format_version_in_code().major,
            network,
        ),
    )?;
    // A snapshot-born node advertises limited service only after retaining a full window
    // above its snapshot base, plus the production finalization margin. Keep H+1 below
    // that base; neither header sync nor a checked-successor download can advance B.
    let mut serving = FinalizedState::new_with_debug(
        destination,
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )?;
    let mut parent = tip_block;
    for _ in 0..retained_blocks {
        let next = context_child(network, &serving, &parent);
        commit(&mut serving, next.clone())?;
        parent = next;
    }
    drop(serving);

    // The serving view advertises the prepared H snapshot, never its temporary context snapshot.
    let original = ArtifactStore::new(source, network);
    let served = ArtifactStore::new(destination, network);
    let manifest_bytes = fs::read(original.object_path(commitment.manifest))?;
    let original_manifest = Manifest::decode(&manifest_bytes, network)?;
    fs::create_dir_all(served.objects_dir())?;
    for hash in std::iter::once(commitment.manifest)
        .chain(original_manifest.pieces().map(|(_, _, piece)| piece.hash))
    {
        let target = served.object_path(hash);
        if !target.exists() {
            // Copies keep the prepared objects independent even if a serving view is modified.
            fs::copy(original.object_path(hash), target)?;
        }
    }
    Ok(final_height)
}

/// Prepares real mock header context without advancing the comparison source.
///
/// A disposable RocksDB checkpoint is advanced using the existing trusted mock
/// commit path. Only its headers through `H + tail` are retained in the source;
/// no tail bodies or changed state rows are published there.
///
/// Returns headers `1..=H + tail`, excluding genesis. The caller must verify
/// them with the production consensus header verifier before publishing checkpoints.
pub fn prepare_header_context(
    config: &Config,
    network: &Network,
    tail: u32,
) -> Result<Vec<Arc<block::Header>>, BoxError> {
    let workspace = tempfile::tempdir_in(&config.cache_dir)?;
    let context_config = Config {
        cache_dir: workspace.path().join("context"),
        ..config.clone()
    };
    let HeaderContext {
        state: context,
        headers,
        source_height,
    } = clone_with_tail(config, network, tail, &context_config)?;
    drop(context);
    let state = FinalizedState::new_with_debug(
        config,
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )?;
    let tail_start = usize::try_from(source_height.0)?;
    for batch in headers[tail_start..].chunks(MAX_COMMIT_HEADERS) {
        commit_headers(&state.db, network, batch.to_vec())?;
    }

    Ok(headers)
}

struct HeaderContext {
    state: FinalizedState,
    headers: Vec<Arc<block::Header>>,
    source_height: Height,
}

/// Clones the source with RocksDB's checkpoint API, then commits inert context blocks.
/// Every mutation is confined to the disposable clone, including its format bookkeeping.
fn clone_with_tail(
    source: &Config,
    network: &Network,
    tail: u32,
    destination: &Config,
) -> Result<HeaderContext, BoxError> {
    if !network.is_regtest() || source.ephemeral || tail == 0 {
        return Err(
            "mock header context requires persistent Regtest state and a positive tail".into(),
        );
    }
    let original = FinalizedState::new_with_debug(
        source,
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )?;
    let source_height = original
        .db
        .tip()
        .filter(|(height, _)| *height >= Height(1))
        .ok_or("mock header context requires a finalized source above genesis")?
        .0;
    let last_height =
        (source_height + i64::from(tail)).ok_or("mock header tail height overflow")?;
    let limit = header_lookahead_limit(&original.db, network).min(MAX_ON_DISK_HEIGHT);
    if last_height > limit {
        return Err(format!(
            "mock header tail ends at {last_height:?}, above lookahead limit {limit:?}"
        )
        .into());
    }
    let target = destination.db_path(
        STATE_DATABASE_KIND,
        state_database_format_version_in_code().major,
        network,
    );
    fs::create_dir_all(target.parent().ok_or("context database has no parent")?)?;
    original.db.checkpoint(&target)?;
    drop(original);
    let mut state = FinalizedState::new_with_debug(
        destination,
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )?;
    state
        .db
        .update_format_version_on_disk(&state_database_format_version_in_code())?;
    state.db.mark_finished_format_upgrades();
    let genesis = state
        .db
        .block_header(Height(0).into())
        .ok_or("missing genesis header")?;
    if genesis.hash() != network.genesis_hash() {
        return Err("mock source has the wrong genesis".into());
    }
    let mut context = VecDeque::from([genesis]);
    let mut headers = Vec::with_capacity(usize::try_from(last_height.0)?);
    for height in 1..=source_height.0 {
        let header = state
            .db
            .block_header(Height(height).into())
            .ok_or("missing source header")?;
        check_header_context(context.make_contiguous(), &header, Height(height), network)?;
        context.push_front(header.clone());
        context.truncate(MAX_POW_ADJUSTMENT_BLOCK_SPAN);
        headers.push(header);
    }
    let mut parent: Arc<Block> = state
        .db
        .block(source_height.into())?
        .ok_or("source tip body is missing")?;
    for height in source_height.0 + 1..=last_height.0 {
        let next = context_child(network, &state, &parent);
        check_header_context(
            context.make_contiguous(),
            &next.header,
            Height(height),
            network,
        )?;
        commit(&mut state, next.clone())?;
        context.push_front(next.header.clone());
        context.truncate(MAX_POW_ADJUSTMENT_BLOCK_SPAN);
        headers.push(next.header.clone());
        parent = next;
    }
    Ok(HeaderContext {
        state,
        headers,
        source_height,
    })
}

/// Builds the same inert block for both cloned and retained serving context.
fn context_child(network: &Network, state: &FinalizedState, parent: &Block) -> Arc<Block> {
    child(
        network,
        state,
        parent,
        vec![transparent::Output {
            value: Amount::zero(),
            lock_script: transparent::Script::new(&[0x6a]),
        }],
        Vec::new(),
    )
}
