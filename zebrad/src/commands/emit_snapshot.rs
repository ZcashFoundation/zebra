//! `emit-snapshot` subcommand - the release-time constants-updater for the
//! P2P-distributed IBD snapshot artifacts.
//!
//! Run against a synced, read-only finalized state, this command regenerates the
//! three deterministic snapshot artifacts from the chain, hashes them, and edits
//! the pinned Zebra source constants in place so a release ships an updated trust
//! root (`docs/design/p2p-snapshot-distribution.md`). It writes **no asset
//! files** by default — the artifacts themselves are served over P2P and
//! verified against these constants. The artifacts are:
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
//! The chunk bytes are regenerated through the *same* read-service function the
//! P2P serve path uses ([`zebra_state::known_hash_chunk_bytes`]), so the emitted
//! hashes and the served bytes can never disagree.
//!
//! The command is **idempotent**: re-running it against a state whose constants
//! are already current changes nothing and prints "already current". The
//! snapshot height is the cached state's finalized tip, so point this at a state
//! synced to the height you want to snapshot.
//!
//! The legacy `.bin` asset emit (unspent-output-locations / tree-roots) is kept
//! behind the hidden `--emit-files` flag for debugging only.

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
use zebra_state::ZebraDb;

use crate::prelude::APPLICATION;

use editor::Change;

/// The name of the emitted unspent-transparent-output-location artifact
/// (only written under `--emit-files`).
const UNSPENT_OUTPUTS_FILE: &str = "unspent-output-locations.bin";

/// The name of the emitted Sapling note-commitment-tree-root artifact
/// (only written under `--emit-files`).
const SAPLING_ROOTS_FILE: &str = "sapling-tree-roots.bin";

/// The name of the emitted Orchard note-commitment-tree-root artifact
/// (only written under `--emit-files`).
const ORCHARD_ROOTS_FILE: &str = "orchard-tree-roots.bin";

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

    /// Debugging only: also write the raw `.bin` asset files into this directory.
    /// The release pipeline does not need these; the artifacts are served over
    /// P2P and verified against the edited constants.
    #[clap(long, help = "ALSO write the raw .bin asset files (debugging only)")]
    emit_files: bool,

    /// Output directory for the `--emit-files` debugging artifacts.
    #[clap(long, short, help = "directory for --emit-files .bin artifacts")]
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
            self.emit_debug_files(&db)?;
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
    /// first generation. A non-deterministic chunk would make peers produce
    /// different bytes for the same span, breaking content-addressing, so the run
    /// aborts rather than pinning an unstable hash.
    ///
    /// This deliberately does **not** compare the recomputed hashes against the
    /// currently-pinned `chunk_hashes` constants: those are the chunk **bytes'**
    /// SHA-256 in whatever format is shipped, and this command's whole purpose is
    /// to (re-)emit them in the v2 format
    /// (`docs/design/p2p-snapshot-distribution.md`). The pinned constants may
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
                 The trust root for the P2P-distributed unspent-output snapshot: a \
                 downloading node fetches the set over P2P and verifies it against this \
                 hash (see `docs/design/p2p-snapshot-distribution.md`). Regenerated by the \
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
                 The trust root for the P2P-distributed address-balance snapshot, loaded at \
                 `H_max` so balances are never recomputed during sync (see \
                 `docs/design/p2p-snapshot-distribution.md`). Regenerated by the \
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

    /// Writes the legacy `.bin` debugging artifacts under `--emit-files`.
    #[allow(clippy::print_stdout)]
    fn emit_debug_files(&self, db: &ZebraDb) -> Result<()> {
        let out_dir = self
            .out_dir
            .clone()
            .ok_or_else(|| eyre!("--emit-files requires --out-dir"))?;
        std::fs::create_dir_all(&out_dir)?;

        let unspent_outputs = emit_unspent_output_locations(db, &out_dir)?;
        let (sapling_roots, orchard_roots) = emit_tree_roots(db, &out_dir)?;

        println!(
            "wrote debugging .bin artifacts:\n  \
             {unspent_outputs} unspent transparent output locations -> {}\n  \
             {sapling_roots} Sapling tree roots -> {}\n  \
             {orchard_roots} Orchard tree roots -> {}",
            out_dir.join(UNSPENT_OUTPUTS_FILE).display(),
            out_dir.join(SAPLING_ROOTS_FILE).display(),
            out_dir.join(ORCHARD_ROOTS_FILE).display(),
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
/// Uses the same byte source the P2P serve path ranges over
/// ([`ZebraDb::for_each_unspent_output_location_bytes`]), so the hash covers the
/// exact bytes peers serve.
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
/// Concatenates each record's `(address_bytes, value_bytes)` exactly as the P2P
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

/// Emits the Sapling and Orchard note-commitment-tree roots at every updating
/// height (debugging only, under `--emit-files`). Returns the two counts.
fn emit_tree_roots(db: &ZebraDb, out_dir: &Path) -> Result<(u64, u64)> {
    let sapling = write_root_records(
        &out_dir.join(SAPLING_ROOTS_FILE),
        db.sapling_tree_by_height_range(..)
            .map(|(height, tree)| (height.0, <[u8; 32]>::from(tree.root()))),
    )?;

    let orchard = write_root_records(
        &out_dir.join(ORCHARD_ROOTS_FILE),
        db.orchard_tree_by_height_range(..)
            .map(|(height, tree)| (height.0, <[u8; 32]>::from(tree.root()))),
    )?;

    Ok((sapling, orchard))
}

/// Streams every unspent transparent output location to the debugging artifact
/// file. Returns the count.
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

/// Writes `(height, root)` records (4-byte little-endian height + 32-byte root)
/// to `path` in iteration order. Returns the number of records written.
fn write_root_records(path: &Path, records: impl Iterator<Item = (u32, [u8; 32])>) -> Result<u64> {
    let mut writer = BufWriter::new(File::create(path)?);

    let mut count: u64 = 0;
    for (height, root) in records {
        writer.write_all(&height.to_le_bytes())?;
        writer.write_all(&root)?;
        count += 1;
    }

    writer.flush()?;
    Ok(count)
}
