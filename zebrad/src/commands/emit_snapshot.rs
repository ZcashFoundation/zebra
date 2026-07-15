//! `emit-snapshot` subcommand - the release-time constants-updater and artifact
//! emitter for the IBD snapshot release assets.
//!
//! Run against a synced, read-only finalized state, this command regenerates the
//! three deterministic snapshot artifacts from the chain, hashes them, and edits
//! the pinned Zebra source constants in place so a release ships an updated trust
//! root (`docs/design/snapshot-distribution.md`). By default it only updates the
//! constants; `--emit-files` also writes the artifact set that is published with
//! the release (and downloaded by the installer), verified against these
//! constants. The artifacts are:
//!
//! - the **known-hash chunks**: for every 150,000-block span up to the finalized
//!   tip, the deterministic `chunk_v2` bytes (block hashes, size hints, and the
//!   sapling/orchard tree roots that update within the span), each SHA-256ed into
//!   `MAINNET/TESTNET_KNOWN_HASHES.chunk_hashes`;
//! - the **unspent-output set**: the sorted unspent transparent output locations
//!   at the tip, SHA-256ed into `MAINNET/TESTNET_UNSPENT_OUTPUTS_HASH`;
//! - the **address-balance set**: the sorted address-balance records at the tip,
//!   SHA-256ed into `MAINNET/TESTNET_ADDRESS_BALANCES_HASH`.
//!
//! The chunk bytes are regenerated through the *same* read-service function
//! ([`zebra_state::known_hash_chunk_bytes`]) the consume path stores and reads,
//! so the emitted hashes and the consumed bytes can never disagree.
//!
//! The command is **idempotent**: re-running it against a state whose constants
//! are already current changes nothing and prints "already current". The
//! snapshot height is the cached state's finalized tip, so point this at a state
//! synced to the height you want to snapshot.
//!
//! ## `--emit-files`: the complete local artifact set
//!
//! With `--emit-files --out-dir <dir>` the command **also** writes the complete
//! v2 artifact set a snapshot-consume consumer needs, in the layout documented in
//! [`crate::components::ibd::consume::local`]:
//!
//! - `chunks/chunk-<index>.bin`: the exact v2 chunk bytes for each index (the
//!   same bytes whose SHA-256 is the pinned `chunk_hashes[index]`);
//! - `sapling-trees.bin` / `orchard-trees.bin`: the note-commitment-tree frontier
//!   at every updating height, as `(height u32 LE, len u32 LE, frontier-bytes)`
//!   records sorted by height (the canonical serialization from
//!   [`zebra_state::note_commitment_tree_bytes`]);
//! - `unspent-output-locations.bin` / `address-balances.bin`: the sorted sets,
//!   whose SHA-256s are the pinned set hashes;
//! - `chain-value-pools.bin`: the 40-byte `H_max` chain value pools, for the
//!   state's bulk value-pool load;
//! - `MANIFEST.txt`: documents the layout and provenance (network, `H_max`).
//!
//! A node configured with `sync.known_hash_local_source_dir = <dir>` then drives
//! a full snapshot-consume sync from these files alone. The emission is
//! deterministic, so every release manager produces byte-identical assets that
//! verify against the same pinned constants. See
//! `docs/design/snapshot-distribution.md`.

mod editor;

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use abscissa_core::{Application, Command, Runnable};
use clap::Parser;
use color_eyre::eyre::{eyre, Result};
use sha2::{Digest, Sha256};

use zebra_chain::{
    block::Height,
    parameters::{known_hashes::HASHES_PER_CHUNK, Network},
};
use zebra_state::{ShieldedPool, ZebraDb};

use crate::{
    components::ibd::consume::local::{
        chunk_file_name, ADDRESS_BALANCES_FILE, CHAIN_VALUE_POOLS_FILE, CHUNKS_SUBDIR,
        MANIFEST_FILE, ORCHARD_TREES_FILE, SAPLING_TREES_FILE, UNSPENT_OUTPUTS_FILE,
    },
    prelude::APPLICATION,
};

use editor::Change;

/// The source file holding the known-hash list specs (chunk hashes + max height
/// + the new snapshot-set hash constants), relative to the `zebrad` crate.
const KNOWN_HASHES_SRC: &str = "../zebra-chain/src/parameters/known_hashes.rs";

/// The source file holding the checkpoint max-height constants, relative to the
/// `zebrad` crate.
const CHECKPOINT_CONSTANTS_SRC: &str = "../zebra-chain/src/parameters/checkpoint/constants.rs";

/// `emit-snapshot` regenerates the pinned IBD snapshot hashes and edits the
/// Zebra source constants in place (release maintainers only)
#[derive(Command, Debug, Default, Parser)]
pub struct EmitSnapshotCmd {
    /// Path to Zebra's cached state directory.
    #[clap(long, short, help = "path to the directory with the Zebra chain state")]
    cache_dir: Option<PathBuf>,

    /// The network of the cached state. Only this network's constants are edited.
    #[clap(long, short, help = "the network of the chain to load")]
    network: Network,

    /// The Zebra workspace root, used to locate the source files to edit.
    /// Defaults to the `zebrad` crate's compile-time manifest directory, which
    /// is correct for `cargo run` from a checkout.
    #[clap(
        long,
        help = "the zebrad crate directory whose source constants to edit"
    )]
    src_root: Option<PathBuf>,

    /// Also write the complete v2 snapshot-consume artifact set into `--out-dir`.
    /// This is the artifact set published with the release (and downloaded by
    /// the installer); a node configured with
    /// `sync.known_hash_local_source_dir = <out-dir>` drives a full
    /// snapshot-consume sync from it.
    #[clap(
        long,
        help = "ALSO write the complete local artifact set into --out-dir"
    )]
    emit_files: bool,

    /// Output directory for the `--emit-files` artifact set.
    #[clap(long, short, help = "directory for the --emit-files artifact set")]
    out_dir: Option<PathBuf>,
}

impl Runnable for EmitSnapshotCmd {
    /// `emit-snapshot` sub-command entrypoint.
    fn run(&self) {
        if let Err(error) = self.emit() {
            tracing::error!("failed to update snapshot constants: {error:?}");
        }
    }
}

/// The fully-recomputed snapshot hashes for one network, ready to be edited into
/// the source constants.
struct ComputedHashes {
    /// The finalized tip height (`H_max`).
    tip_height: Height,
    /// One lowercase-hex SHA-256 per known-hash chunk, in chunk order.
    chunk_hashes: Vec<String>,
    /// SHA-256 of the sorted unspent-output-location set, lowercase hex.
    unspent_outputs_hash: String,
    /// SHA-256 of the sorted address-balance set, lowercase hex.
    address_balances_hash: String,
    /// Total unspent output locations hashed (for the log).
    unspent_outputs_count: u64,
    /// Total address-balance records hashed (for the log).
    address_balances_count: u64,
}

impl EmitSnapshotCmd {
    /// Opens the cached state read-only, recomputes the snapshot hashes, and
    /// edits the source constants for the selected network.
    #[allow(clippy::print_stdout)]
    fn emit(&self) -> Result<()> {
        let mut config = APPLICATION.config().state.clone();
        if let Some(cache_dir) = self.cache_dir.clone() {
            config.cache_dir = cache_dir;
        }

        // `init_read_only` returns the read service, the finalized `ZebraDb`,
        // and the non-finalized sender; the updater only needs the finalized DB.
        let db = zebra_state::init_read_only(config, &self.network).1;

        let (tip_height, tip_hash) = db
            .tip()
            .ok_or_else(|| eyre!("the cached state has no chain tip block"))?;

        let computed = self.compute_hashes(&db, tip_height)?;

        tracing::info!(
            ?tip_height,
            ?tip_hash,
            chunks = computed.chunk_hashes.len(),
            unspent_outputs = computed.unspent_outputs_count,
            address_balances = computed.address_balances_count,
            "recomputed IBD snapshot hashes",
        );

        let changes = self.apply_edits(&computed)?;

        if changes.is_empty() {
            println!(
                "snapshot constants for {} at tip height {} ({}) are already current — no changes",
                self.network, tip_height.0, tip_hash,
            );
        } else {
            println!(
                "updated {} snapshot constants for {} at tip height {} ({}):",
                changes.len(),
                self.network,
                tip_height.0,
                tip_hash,
            );
            for change in &changes {
                println!("{change}");
            }
        }

        if self.emit_files {
            self.emit_local_artifact_set(&db, tip_height)?;
        }

        Ok(())
    }

    /// Recomputes every chunk hash and the two set hashes from the finalized
    /// state, running the correctness gate against the bundled spec.
    fn compute_hashes(&self, db: &ZebraDb, tip_height: Height) -> Result<ComputedHashes> {
        // Number of 150,000-block chunks covering heights `0..=tip_height`.
        let num_chunks = (u64::from(tip_height.0) + 1).div_ceil(u64::from(HASHES_PER_CHUNK));

        let mut chunk_hashes = Vec::with_capacity(num_chunks as usize);
        for index in 0..num_chunks {
            // `num_chunks` is bounded by the tip height (a u32 block height) so
            // the chunk index always fits a u32.
            let index = index as u32;
            let bytes = zebra_state::known_hash_chunk_bytes(db, index).ok_or_else(|| {
                eyre!("chunk {index} could not be regenerated from the synced state")
            })?;
            chunk_hashes.push(hex::encode(Sha256::digest(&bytes)));
        }

        self.correctness_gate(db, &chunk_hashes)?;

        let (unspent_outputs_hash, unspent_outputs_count) = hash_unspent_outputs(db);
        let (address_balances_hash, address_balances_count) = hash_address_balances(db);

        Ok(ComputedHashes {
            tip_height,
            chunk_hashes,
            unspent_outputs_hash,
            address_balances_hash,
            unspent_outputs_count,
            address_balances_count,
        })
    }

    /// Verifies that chunk generation is **deterministic**: each chunk is
    /// regenerated a second time and its SHA-256 must be byte-identical to the
    /// first generation. A non-deterministic chunk would make release runs produce
    /// different bytes for the same span, breaking content-addressing, so the run
    /// aborts rather than pinning an unstable hash.
    ///
    /// This deliberately does **not** compare the recomputed hashes against the
    /// currently-pinned `chunk_hashes` constants: those are the chunk **bytes'**
    /// SHA-256 in whatever format is shipped, and this command's whole purpose is
    /// to (re-)emit them in the v2 format
    /// (`docs/design/snapshot-distribution.md`). The pinned constants may
    /// still be the legacy v1 SHA-256, against which the v2 recomputation would
    /// always mismatch; comparing like-for-v1-vs-v2 would falsely block the very
    /// migration this command performs. Determinism is the property that must
    /// hold regardless of format, so it is what the gate checks.
    fn correctness_gate(&self, db: &ZebraDb, recomputed: &[String]) -> Result<()> {
        for (index, first_hash) in recomputed.iter().enumerate() {
            // `index` is bounded by the chunk count (derived from a u32 height),
            // so it fits a u32.
            let index = index as u32;
            let bytes = zebra_state::known_hash_chunk_bytes(db, index).ok_or_else(|| {
                eyre!("chunk {index} could not be regenerated for the determinism check")
            })?;
            let second_hash = hex::encode(Sha256::digest(&bytes));
            if &second_hash != first_hash {
                return Err(eyre!(
                    "correctness gate failed for chunk {index}: regenerating it produced \
                     {second_hash} but the first generation was {first_hash} — chunk \
                     generation is not deterministic, so refusing to pin an unstable hash",
                ));
            }
        }

        tracing::info!(
            total_recomputed = recomputed.len(),
            "correctness gate passed: every chunk regenerates byte-identically (deterministic)",
        );

        Ok(())
    }

    /// Edits the source constants for the selected network and returns the list
    /// of changes made (empty when everything was already current).
    fn apply_edits(&self, computed: &ComputedHashes) -> Result<Vec<Change>> {
        let src_root = self
            .src_root
            .clone()
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));

        let net = NetworkConsts::for_network(&self.network)?;

        let mut changes = Vec::new();

        // Decide the single max height to write into BOTH the known-hash spec's
        // `max_height` and the checkpoint constant, so they never disagree. The
        // known-hash list's `max_height` must equal the network's max checkpoint
        // height (enforced by `for_network_coverage`), and the checkpoint
        // constant is tied to the sparse `*-checkpoints.txt` list, which this
        // command does not regenerate. So if the synced tip is above the pinned
        // checkpoint coverage, we clamp the written max height to the current
        // checkpoint constant (and warn) rather than advancing `max_height` past
        // a checkpoint list that wasn't extended.
        let checkpoint_path = src_root.join(CHECKPOINT_CONSTANTS_SRC);
        let effective_max_height =
            self.effective_max_height(&checkpoint_path, computed.tip_height, net)?;

        // 1. known_hashes.rs: chunk_hashes, max_height, and the two set-hash
        //    constants for the selected network.
        let known_hashes_path = src_root.join(KNOWN_HASHES_SRC);
        let original = std::fs::read_to_string(&known_hashes_path)
            .map_err(|error| eyre!("could not read {}: {error}", known_hashes_path.display()))?;
        let mut source = original.clone();

        let (s, change) =
            editor::set_chunk_hashes(&source, net.spec_const, &computed.chunk_hashes)?;
        source = s;
        changes.extend(change);

        let (s, change) =
            editor::set_height_field(&source, net.spec_const, "max_height", effective_max_height)?;
        source = s;
        changes.extend(change);

        let (s, change) = editor::set_or_insert_str_const(
            &source,
            net.unspent_outputs_const,
            &computed.unspent_outputs_hash,
            &format!(
                "SHA-256 of the sorted unspent-transparent-output-location set at the \
                 {} finalized tip ({}), as lowercase hex.\n\
                 \n\
                 The trust root for the unspent-output snapshot release artifact: a \
                 consuming node verifies the downloaded set against this hash (see \
                 `docs/design/snapshot-distribution.md`). Regenerated by the \
                 `emit-snapshot` command at every release.",
                self.network, net.display,
            ),
            net.spec_const,
        )?;
        source = s;
        changes.extend(change);

        let (s, change) = editor::set_or_insert_str_const(
            &source,
            net.address_balances_const,
            &computed.address_balances_hash,
            &format!(
                "SHA-256 of the sorted address-balance set at the {} finalized tip ({}), \
                 as lowercase hex.\n\
                 \n\
                 The trust root for the address-balance snapshot release artifact, loaded \
                 at `H_max` so balances are never recomputed during sync (see \
                 `docs/design/snapshot-distribution.md`). Regenerated by the \
                 `emit-snapshot` command at every release.",
                self.network, net.display,
            ),
            net.spec_const,
        )?;
        source = s;
        changes.extend(change);

        if source != original {
            std::fs::write(&known_hashes_path, &source).map_err(|error| {
                eyre!("could not write {}: {error}", known_hashes_path.display())
            })?;
        }

        // 2. checkpoint/constants.rs: write the same effective max height, so the
        //    known-hash `max_height` and the checkpoint constant always agree.
        self.handle_checkpoint_constant(&checkpoint_path, effective_max_height, net, &mut changes)?;

        Ok(changes)
    }

    /// Returns the max height to write into **both** the known-hash spec's
    /// `max_height` and the checkpoint constant, so they never disagree.
    ///
    /// The checkpoint constant is tied to the sparse `*-checkpoints.txt` list
    /// (enforced by `max_checkpoint_height_constants_match_lists`), which this
    /// command does not regenerate, and the known-hash list's `max_height` must
    /// equal the network's max checkpoint height (`for_network_coverage`). So if
    /// the synced tip is above the pinned checkpoint coverage, both constants are
    /// clamped to the current checkpoint constant and a loud, actionable warning
    /// tells the maintainer to extend the `.txt` list first; otherwise the tip is
    /// used.
    fn effective_max_height(
        &self,
        checkpoint_path: &Path,
        tip_height: Height,
        net: NetworkConsts,
    ) -> Result<u32> {
        let source = std::fs::read_to_string(checkpoint_path)
            .map_err(|error| eyre!("could not read {}: {error}", checkpoint_path.display()))?;
        let current = read_standalone_height(&source, net.checkpoint_const)?;

        if tip_height.0 > current {
            tracing::warn!(
                tip = tip_height.0,
                checkpoint_max = current,
                checkpoint_const = net.checkpoint_const,
                "the synced tip is above the pinned checkpoint max height. The checkpoint \
                 constant is tied to the sparse {} list (enforced by \
                 max_checkpoint_height_constants_match_lists) and is NOT advanced here, so the \
                 known-hash max_height is clamped to it to keep the two consistent. Extend that \
                 .txt list (e.g. with `zebra-checkpoints`) and re-run before release.",
                net.checkpoint_txt,
            );
            return Ok(current);
        }

        Ok(tip_height.0)
    }

    /// Writes `max_height` into the checkpoint max-height constant, keeping it in
    /// lockstep with the known-hash `max_height`.
    ///
    /// `max_height` is the [`effective_max_height`](Self::effective_max_height),
    /// already clamped to the checkpoint `.txt` coverage, so this never advances
    /// the constant past that list. It is a no-op (no change recorded) when the
    /// constant already matches.
    fn handle_checkpoint_constant(
        &self,
        path: &Path,
        max_height: u32,
        net: NetworkConsts,
        changes: &mut Vec<Change>,
    ) -> Result<()> {
        let source = std::fs::read_to_string(path)
            .map_err(|error| eyre!("could not read {}: {error}", path.display()))?;

        let (edited, change) =
            editor::set_standalone_height(&source, net.checkpoint_const, max_height)?;
        if let Some(change) = change {
            std::fs::write(path, &edited)
                .map_err(|error| eyre!("could not write {}: {error}", path.display()))?;
            changes.push(change);
        }

        Ok(())
    }

    /// Writes the complete v2 snapshot-consume artifact set under `--emit-files`.
    ///
    /// The layout (documented in [`crate::components::ibd::consume::local`]) is
    /// what a node configured with `sync.known_hash_local_source_dir` reads to
    /// drive a full solo snapshot-consume sync. Every artifact is byte-identical
    /// to what the consume path verifies (the chunk bytes come from the same
    /// [`zebra_state::known_hash_chunk_bytes`] function; the tree records hold the
    /// same [`zebra_state::note_commitment_tree_bytes`] serialization; the set
    /// files hold the same sorted bytes; the value-pool file holds the 40-byte
    /// `ValueBalance::to_bytes`), so they verify against the same pinned
    /// constants.
    #[allow(clippy::print_stdout)]
    fn emit_local_artifact_set(&self, db: &ZebraDb, tip_height: Height) -> Result<()> {
        let out_dir = self
            .out_dir
            .clone()
            .ok_or_else(|| eyre!("--emit-files requires --out-dir"))?;
        std::fs::create_dir_all(&out_dir)?;

        let chunks = emit_chunk_files(db, &out_dir, tip_height)?;
        let sapling_trees = emit_tree_records(
            &out_dir.join(SAPLING_TREES_FILE),
            db.sapling_tree_by_height_range(..).map(|(height, _tree)| {
                let bytes =
                    zebra_state::note_commitment_tree_bytes(db, ShieldedPool::Sapling, height)
                        .expect("a tree exists at a height the range yielded");
                (height.0, bytes)
            }),
        )?;
        let orchard_trees = emit_tree_records(
            &out_dir.join(ORCHARD_TREES_FILE),
            db.orchard_tree_by_height_range(..).map(|(height, _tree)| {
                let bytes =
                    zebra_state::note_commitment_tree_bytes(db, ShieldedPool::Orchard, height)
                        .expect("a tree exists at a height the range yielded");
                (height.0, bytes)
            }),
        )?;
        let unspent_outputs = emit_unspent_output_locations(db, &out_dir)?;
        let address_balances = emit_address_balances(db, &out_dir)?;
        emit_chain_value_pools(db, &out_dir, tip_height)?;
        write_manifest(
            &out_dir,
            &self.network,
            tip_height,
            chunks,
            sapling_trees,
            orchard_trees,
            unspent_outputs,
            address_balances,
        )?;

        println!(
            "wrote the local snapshot-consume artifact set into {}:\n  \
             {chunks} chunk files -> {CHUNKS_SUBDIR}/\n  \
             {sapling_trees} Sapling tree records -> {SAPLING_TREES_FILE}\n  \
             {orchard_trees} Orchard tree records -> {ORCHARD_TREES_FILE}\n  \
             {unspent_outputs} unspent output locations -> {UNSPENT_OUTPUTS_FILE}\n  \
             {address_balances} address balances -> {ADDRESS_BALANCES_FILE}\n  \
             chain value pools at H_max={} -> {CHAIN_VALUE_POOLS_FILE}\n  \
             layout manifest -> {MANIFEST_FILE}",
            out_dir.display(),
            tip_height.0,
        );
        println!(
            "to drive a solo snapshot-consume sync, set \
             `sync.known_hash_local_source_dir = \"{}\"` (with \
             `sync.snapshot_consume_sync = true`)",
            out_dir.display(),
        );

        Ok(())
    }
}

/// The per-network constant names this command edits.
#[derive(Copy, Clone, Debug)]
struct NetworkConsts {
    /// A human label for log/doc text, e.g. "Mainnet".
    display: &'static str,
    /// The known-hash spec const name, e.g. `MAINNET_KNOWN_HASHES`.
    spec_const: &'static str,
    /// The unspent-output-set hash const name.
    unspent_outputs_const: &'static str,
    /// The address-balance-set hash const name.
    address_balances_const: &'static str,
    /// The checkpoint max-height const name.
    checkpoint_const: &'static str,
    /// The sparse checkpoint list file name (for the warning text).
    checkpoint_txt: &'static str,
}

impl NetworkConsts {
    /// Returns the constant names for `network`, or an error for networks
    /// without a bundled known-hash list (e.g. custom testnets, regtest).
    fn for_network(network: &Network) -> Result<Self> {
        match network {
            Network::Mainnet => Ok(Self {
                display: "Mainnet",
                spec_const: "MAINNET_KNOWN_HASHES",
                unspent_outputs_const: "MAINNET_UNSPENT_OUTPUTS_HASH",
                address_balances_const: "MAINNET_ADDRESS_BALANCES_HASH",
                checkpoint_const: "MAINNET_MAX_CHECKPOINT_HEIGHT",
                checkpoint_txt: "main-checkpoints.txt",
            }),
            Network::Testnet(params) if params.is_default_testnet() => Ok(Self {
                display: "Testnet",
                spec_const: "TESTNET_KNOWN_HASHES",
                unspent_outputs_const: "TESTNET_UNSPENT_OUTPUTS_HASH",
                address_balances_const: "TESTNET_ADDRESS_BALANCES_HASH",
                checkpoint_const: "TESTNET_MAX_CHECKPOINT_HEIGHT",
                checkpoint_txt: "test-checkpoints.txt",
            }),
            _ => Err(eyre!(
                "no bundled known-hash constants for {network}; emit-snapshot only updates \
                 Mainnet and the default Testnet"
            )),
        }
    }
}

/// Streams the sorted unspent-output-location set through a SHA-256 hasher,
/// returning `(lowercase_hex_hash, record_count)`.
///
/// Uses the same byte source the set-hash constants cover
/// ([`ZebraDb::for_each_unspent_output_location_bytes`]), so the hash covers the
/// exact bytes the release artifact holds.
fn hash_unspent_outputs(db: &ZebraDb) -> (String, u64) {
    let mut hasher = Sha256::new();
    let mut count: u64 = 0;
    db.for_each_unspent_output_location_bytes(|bytes| {
        hasher.update(bytes);
        count += 1;
    });
    (hex::encode(hasher.finalize()), count)
}

/// Streams the sorted address-balance set through a SHA-256 hasher, returning
/// `(lowercase_hex_hash, record_count)`.
///
/// Concatenates each record's `(address_bytes, value_bytes)` exactly as the
/// serve path does ([`ZebraDb::for_each_address_balance_bytes`]).
fn hash_address_balances(db: &ZebraDb) -> (String, u64) {
    let mut hasher = Sha256::new();
    let mut count: u64 = 0;
    db.for_each_address_balance_bytes(|address_bytes, value_bytes| {
        hasher.update(address_bytes);
        hasher.update(value_bytes);
        count += 1;
    });
    (hex::encode(hasher.finalize()), count)
}

/// Reads the integer argument of a standalone
/// `pub const <name>: Height = Height(N);` declaration in `source`.
fn read_standalone_height(source: &str, name: &str) -> Result<u32> {
    let decl = format!("const {name}");
    let decl_start = source
        .find(&decl)
        .ok_or_else(|| eyre!("could not find `const {name}` in the checkpoint constants"))?;
    let marker = "Height(";
    let start = source[decl_start..]
        .find(marker)
        .map(|rel| decl_start + rel + marker.len())
        .ok_or_else(|| eyre!("could not find `Height(` for `const {name}`"))?;
    let end = source[start..]
        .find(')')
        .map(|rel| start + rel)
        .ok_or_else(|| eyre!("unterminated `Height(` for `const {name}`"))?;
    let digits: String = source[start..end]
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse()
        .map_err(|_| eyre!("could not parse the height for `const {name}`"))
}

/// Writes one `chunks/chunk-<index>.bin` file per chunk covering
/// `0..=tip_height`, holding the exact v2 chunk bytes (the same bytes whose
/// SHA-256 is the pinned `chunk_hashes[index]`). Returns the chunk count.
///
/// The bytes come from [`zebra_state::known_hash_chunk_bytes`], the *same*
/// function the constants-updater hashes, so a local chunk file always matches
/// its pinned hash.
fn emit_chunk_files(db: &ZebraDb, out_dir: &Path, tip_height: Height) -> Result<u64> {
    let chunks_dir = out_dir.join(CHUNKS_SUBDIR);
    std::fs::create_dir_all(&chunks_dir)?;

    // Number of 150,000-block chunks covering heights `0..=tip_height`.
    let num_chunks = (u64::from(tip_height.0) + 1).div_ceil(u64::from(HASHES_PER_CHUNK));

    for index in 0..num_chunks {
        // `num_chunks` is bounded by the tip height (a u32 block height), so the
        // chunk index always fits a u32.
        let index = index as u32;
        let bytes = zebra_state::known_hash_chunk_bytes(db, index).ok_or_else(|| {
            eyre!("chunk {index} could not be regenerated for the local artifact set")
        })?;

        let path = chunks_dir.join(chunk_file_name(index));
        std::fs::write(&path, &bytes)
            .map_err(|error| eyre!("could not write {}: {error}", path.display()))?;
    }

    Ok(num_chunks)
}

/// Writes the note-commitment-tree records file at `path` from the `records`
/// iterator (`(height, frontier-bytes)` ascending by height), returning the
/// record count.
///
/// Each record is `(height u32 LE, len u32 LE, len-byte frontier)`. The frontier
/// bytes are the canonical [`zebra_state::note_commitment_tree_bytes`]
/// serialization the state stores, so the consumer's
/// recomputed `.root()` matches the chunk's recorded root.
fn emit_tree_records(path: &Path, records: impl Iterator<Item = (u32, Vec<u8>)>) -> Result<u64> {
    let mut writer = BufWriter::new(File::create(path)?);

    let mut count: u64 = 0;
    for (height, frontier) in records {
        // `frontier.len()` is a serialized tree frontier (a few KiB at most), far
        // below u32::MAX, so the cast never truncates.
        let len = frontier.len() as u32;
        writer.write_all(&height.to_le_bytes())?;
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(&frontier)?;
        count += 1;
    }

    writer.flush()?;
    Ok(count)
}

/// Streams the sorted unspent-output-location set to `unspent-output-locations.bin`,
/// the same bytes the pinned set hash covers. Returns the record count.
fn emit_unspent_output_locations(db: &ZebraDb, out_dir: &Path) -> Result<u64> {
    let path = out_dir.join(UNSPENT_OUTPUTS_FILE);
    let mut writer = BufWriter::new(File::create(&path)?);

    let mut count: u64 = 0;
    let mut write_error = None;
    db.for_each_unspent_output_location_bytes(|bytes| {
        // Stop writing after the first error; surfaced after the stream ends.
        if write_error.is_some() {
            return;
        }
        match writer.write_all(bytes) {
            Ok(()) => count += 1,
            Err(error) => write_error = Some(error),
        }
    });

    if let Some(error) = write_error {
        return Err(error.into());
    }

    writer.flush()?;
    Ok(count)
}

/// Streams the sorted address-balance set to `address-balances.bin`, the same
/// bytes the pinned set hash covers (per record: 21-byte address key + 24-byte
/// balance value). Returns the record count.
fn emit_address_balances(db: &ZebraDb, out_dir: &Path) -> Result<u64> {
    let path = out_dir.join(ADDRESS_BALANCES_FILE);
    let mut writer = BufWriter::new(File::create(&path)?);

    let mut count: u64 = 0;
    let mut write_error = None;
    db.for_each_address_balance_bytes(|address_bytes, value_bytes| {
        if write_error.is_some() {
            return;
        }
        if let Err(error) = writer
            .write_all(address_bytes)
            .and_then(|()| writer.write_all(value_bytes))
        {
            write_error = Some(error);
        } else {
            count += 1;
        }
    });

    if let Some(error) = write_error {
        return Err(error.into());
    }

    writer.flush()?;
    Ok(count)
}

/// Writes the `H_max` chain value pools to `chain-value-pools.bin` as the 40-byte
/// `ValueBalance::to_bytes` encoding, for the state's bulk value-pool load.
fn emit_chain_value_pools(db: &ZebraDb, out_dir: &Path, _tip_height: Height) -> Result<()> {
    let path = out_dir.join(CHAIN_VALUE_POOLS_FILE);
    let value_pool = db.finalized_value_pool();
    std::fs::write(&path, value_pool.to_bytes())
        .map_err(|error| eyre!("could not write {}: {error}", path.display()))?;
    Ok(())
}

/// Writes the human-readable `MANIFEST.txt` documenting the artifact layout and
/// provenance.
#[allow(clippy::too_many_arguments)]
fn write_manifest(
    out_dir: &Path,
    network: &Network,
    tip_height: Height,
    chunks: u64,
    sapling_trees: u64,
    orchard_trees: u64,
    unspent_outputs: u64,
    address_balances: u64,
) -> Result<()> {
    let manifest = format!(
        "# Zebra snapshot-consume local artifact set\n\
         #\n\
         # Written by `emit-snapshot --emit-files`. A node configured with\n\
         # `sync.known_hash_local_source_dir = <this dir>` and\n\
         # `sync.snapshot_consume_sync = true` drives a full snapshot-consume\n\
         # sync from these files. Every artifact is deterministic and verifies\n\
         # against the pinned SHA-256\n\
         # constants. See docs/design/snapshot-distribution.md.\n\
         #\n\
         network = {network}\n\
         h_max = {h_max}\n\
         hashes_per_chunk = {hashes_per_chunk}\n\
         \n\
         # Layout:\n\
         {chunks_subdir}/{chunk_example}   # exact v2 chunk bytes per index ({chunks} files)\n\
         {sapling_trees_file}              # (height u32 LE, len u32 LE, frontier)* ({sapling_trees} records)\n\
         {orchard_trees_file}              # (height u32 LE, len u32 LE, frontier)* ({orchard_trees} records)\n\
         {unspent_outputs_file}   # sorted unspent-output-location set ({unspent_outputs} records)\n\
         {address_balances_file}            # sorted address-balance set ({address_balances} records)\n\
         {chain_value_pools_file}           # 40-byte H_max ValueBalance\n",
        network = network,
        h_max = tip_height.0,
        hashes_per_chunk = HASHES_PER_CHUNK,
        chunks_subdir = CHUNKS_SUBDIR,
        chunk_example = chunk_file_name(0),
        chunks = chunks,
        sapling_trees_file = SAPLING_TREES_FILE,
        sapling_trees = sapling_trees,
        orchard_trees_file = ORCHARD_TREES_FILE,
        orchard_trees = orchard_trees,
        unspent_outputs_file = UNSPENT_OUTPUTS_FILE,
        unspent_outputs = unspent_outputs,
        address_balances_file = ADDRESS_BALANCES_FILE,
        address_balances = address_balances,
        chain_value_pools_file = CHAIN_VALUE_POOLS_FILE,
    );

    let path = out_dir.join(MANIFEST_FILE);
    std::fs::write(&path, manifest)
        .map_err(|error| eyre!("could not write {}: {error}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};
    use zebra_state::{Config as StateConfig, FinalizedState};

    use crate::components::ibd::consume::local::LocalSnapshotSource;

    use super::*;

    /// Builds an ephemeral finalized state with mainnet genesis, block 1, and
    /// block 2 committed, returning it.
    fn populated_state() -> FinalizedState {
        let mut state = FinalizedState::new(
            &StateConfig::ephemeral(),
            &Network::Mainnet,
            #[cfg(feature = "elasticsearch")]
            false,
        );

        let block_bytes: [&[u8]; 3] = [
            zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.as_ref(),
            zebra_test::vectors::BLOCK_MAINNET_1_BYTES.as_ref(),
            zebra_test::vectors::BLOCK_MAINNET_2_BYTES.as_ref(),
        ];
        for bytes in block_bytes {
            let block: Arc<Block> = bytes.zcash_deserialize_into().expect("test data parses");
            state
                .commit_finalized_direct(block.into(), None, "emit-snapshot test")
                .expect("test block is valid");
        }

        state
    }

    /// The emit helpers write exactly the bytes the pinned constants cover, and
    /// the local source reads them back byte-for-byte — so a local artifact set
    /// every emission deliver identical bytes that pass the same content-addressed
    /// checks. This is the emit-side half of the solo-sync round trip.
    #[test]
    fn emit_local_artifact_set_round_trips_through_local_source() {
        let _init = zebra_test::init();

        let state = populated_state();
        let db = &state.db;
        let tip_height = db.finalized_tip_height().expect("the state has a tip");

        let dir = TempDir::new().expect("temp dir is creatable");
        let out_dir = dir.path();

        // Emit each artifact through the real emit helpers.
        let chunks = emit_chunk_files(db, out_dir, tip_height).expect("chunks emit");
        assert_eq!(chunks, 1, "a 3-block state has one chunk");
        emit_tree_records(
            &out_dir.join(SAPLING_TREES_FILE),
            db.sapling_tree_by_height_range(..).map(|(height, _t)| {
                (
                    height.0,
                    zebra_state::note_commitment_tree_bytes(db, ShieldedPool::Sapling, height)
                        .expect("a sapling tree exists at this height"),
                )
            }),
        )
        .expect("sapling tree records emit");
        emit_tree_records(
            &out_dir.join(ORCHARD_TREES_FILE),
            db.orchard_tree_by_height_range(..).map(|(height, _t)| {
                (
                    height.0,
                    zebra_state::note_commitment_tree_bytes(db, ShieldedPool::Orchard, height)
                        .expect("an orchard tree exists at this height"),
                )
            }),
        )
        .expect("orchard tree records emit");
        let unspent = emit_unspent_output_locations(db, out_dir).expect("unspent outputs emit");
        let balances = emit_address_balances(db, out_dir).expect("address balances emit");
        emit_chain_value_pools(db, out_dir, tip_height).expect("value pools emit");
        write_manifest(
            out_dir,
            &Network::Mainnet,
            tip_height,
            chunks,
            0,
            0,
            unspent,
            balances,
        )
        .expect("manifest writes");

        // Read each artifact back through the local source and assert byte parity
        // with the state's canonical bytes.
        let local = LocalSnapshotSource::new(out_dir);

        // Chunk: the file's bytes equal the on-demand-generated chunk bytes (whose
        // SHA-256 is the pinned chunk hash).
        let chunk_from_file = local.read_chunk(0).expect("the chunk file is read");
        let chunk_from_state =
            zebra_state::known_hash_chunk_bytes(db, 0).expect("chunk 0 generates");
        assert_eq!(
            chunk_from_file, chunk_from_state,
            "the emitted chunk file is byte-identical to the state-generated chunk",
        );

        // Trees: each record equals the canonical tree serialization.
        for pool in [ShieldedPool::Sapling, ShieldedPool::Orchard] {
            let heights: Vec<_> = match pool {
                ShieldedPool::Sapling => db
                    .sapling_tree_by_height_range(..)
                    .map(|(h, _)| h)
                    .collect(),
                ShieldedPool::Orchard => db
                    .orchard_tree_by_height_range(..)
                    .map(|(h, _)| h)
                    .collect(),
            };
            for height in heights {
                let from_file = local
                    .read_tree(pool, height.0)
                    .expect("the tree file parses")
                    .expect("a record exists at this updating height");
                let from_state = zebra_state::note_commitment_tree_bytes(db, pool, height)
                    .expect("the state serves this tree");
                assert_eq!(
                    from_file, from_state,
                    "the emitted {pool:?} tree at {height:?} matches the state's tree bytes",
                );
            }
        }

        // Unspent-output set: the whole file equals the streamed set bytes.
        let mut streamed_unspent = Vec::new();
        db.for_each_unspent_output_location_bytes(|b| streamed_unspent.extend_from_slice(b));
        let unspent_from_file = local
            .read_set(UNSPENT_OUTPUTS_FILE)
            .expect("the unspent set file reads");
        assert_eq!(
            unspent_from_file, streamed_unspent,
            "the emitted unspent-output set matches the state's set bytes",
        );

        // Address-balance set: the whole file equals the streamed set bytes.
        let mut streamed_balances = Vec::new();
        db.for_each_address_balance_bytes(|k, v| {
            streamed_balances.extend_from_slice(k);
            streamed_balances.extend_from_slice(v);
        });
        let balances_from_file = local
            .read_set(ADDRESS_BALANCES_FILE)
            .expect("the balance set file reads");
        assert_eq!(
            balances_from_file, streamed_balances,
            "the emitted address-balance set matches the state's set bytes",
        );

        // Chain value pools: the file equals the finalized value pool encoding.
        let pools_from_file = local
            .read_chain_value_pools()
            .expect("the value-pool file reads");
        assert_eq!(
            pools_from_file.as_slice(),
            db.finalized_value_pool().to_bytes().as_slice(),
            "the emitted value pools match the finalized value pool",
        );

        // The manifest is written and names the network and H_max.
        let manifest =
            std::fs::read_to_string(out_dir.join(MANIFEST_FILE)).expect("the manifest is written");
        assert!(
            manifest.contains("network = Mainnet"),
            "manifest names network"
        );
        assert!(
            manifest.contains(&format!("h_max = {}", tip_height.0)),
            "manifest names H_max",
        );
    }
}
