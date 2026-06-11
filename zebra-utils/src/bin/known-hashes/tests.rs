//! Tests for the known-hashes assembly tool.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use color_eyre::eyre::{eyre, Result};
use hex::FromHex;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use zebra_chain::{
    block::{self, MAX_BLOCK_BYTES},
    parameters::Network,
};

use crate::{
    emit::{emit_assets, size_hint, spec_constant_block, HASHES_PER_CHUNK, SIZE_HINT_UNIT},
    source::BlockSource,
    sweep::{run_sweep, SweepFiles},
};

/// A [`BlockSource`] over a fixed in-memory chain, counting every fetch.
#[derive(Clone)]
struct MockSource {
    /// The mock chain: height to block hash and serialized size.
    blocks: Arc<BTreeMap<u32, (block::Hash, u32)>>,

    /// The total number of hash and size fetches.
    calls: Arc<AtomicUsize>,
}

impl MockSource {
    fn new(blocks: BTreeMap<u32, (block::Hash, u32)>) -> Self {
        Self {
            blocks: Arc::new(blocks),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl BlockSource for MockSource {
    async fn tip_height(&self) -> Result<u32> {
        self.blocks
            .keys()
            .next_back()
            .copied()
            .ok_or_else(|| eyre!("the mock chain is empty"))
    }

    async fn block_hash(&self, height: u32) -> Result<block::Hash> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.blocks
            .get(&height)
            .map(|(hash, _size)| *hash)
            .ok_or_else(|| eyre!("no mock block at height {height}"))
    }

    async fn block_size(&self, height: u32) -> Result<u32> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.blocks
            .get(&height)
            .map(|(_hash, size)| *size)
            .ok_or_else(|| eyre!("no mock block at height {height}"))
    }
}

/// A 5-block mock mainnet chain whose genesis block matches the real mainnet
/// genesis hash, so the height 0 checkpoint anchor verifies.
fn mock_mainnet_chain() -> BTreeMap<u32, (block::Hash, u32)> {
    let mut blocks = BTreeMap::new();
    blocks.insert(0, (Network::Mainnet.genesis_hash(), 1_687));
    for height in 1..=4u32 {
        let byte = u8::try_from(height).expect("test heights are below 256");
        blocks.insert(height, (block::Hash([byte; 32]), 1_000 + height));
    }
    blocks
}

#[test]
fn size_hint_unit_matches_spec() {
    // §6.2: `SIZE_HINT_UNIT = MAX_BLOCK_BYTES.div_ceil(255)` = 7,844 bytes.
    assert_eq!(SIZE_HINT_UNIT, 7_844);
    assert_eq!(u64::from(SIZE_HINT_UNIT), MAX_BLOCK_BYTES.div_ceil(255));
}

#[test]
fn size_hint_quantization() {
    let max_block_bytes = u32::try_from(MAX_BLOCK_BYTES).expect("the protocol limit fits in a u32");

    assert_eq!(size_hint(1).unwrap(), 1);
    // Exact multiples of the unit must not round up to the next hint.
    assert_eq!(size_hint(SIZE_HINT_UNIT).unwrap(), 1);
    assert_eq!(size_hint(SIZE_HINT_UNIT + 1).unwrap(), 2);
    assert_eq!(size_hint(3 * SIZE_HINT_UNIT).unwrap(), 3);
    assert_eq!(size_hint(3 * SIZE_HINT_UNIT + 1).unwrap(), 4);
    assert_eq!(size_hint(max_block_bytes).unwrap(), 255);

    // Zero marks an unswept height, and oversized blocks are invalid.
    assert!(size_hint(0).is_err());
    assert!(size_hint(max_block_bytes + 1).is_err());
}

#[test]
fn display_hex_parses_to_internal_byte_order() {
    let display = "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08";

    let hash: block::Hash = display.parse().expect("valid block hash hex");

    // RPC responses are display-order hex; the internal order is its reverse.
    let mut expected = <[u8; 32]>::from_hex(display).expect("valid hex");
    expected.reverse();

    assert_eq!(hash.0, expected);
    assert_eq!(hash, Network::Mainnet.genesis_hash());
    assert_eq!(hash.to_string(), display);
}

#[test]
fn chunking_splits_at_exact_chunk_boundary() -> Result<()> {
    // Exactly one full chunk must not create an empty second chunk.
    let dir = tempdir()?;
    let hashes = vec![[1u8; 32]; HASHES_PER_CHUNK];
    let hints = vec![1u8; HASHES_PER_CHUNK];

    let spec = emit_assets(dir.path(), "main", &hashes, &hints)?;

    assert_eq!(spec.chunk_hashes.len(), 1);
    let chunk = std::fs::read(dir.path().join("main-known-hashes-00.bin"))?;
    assert_eq!(chunk.len(), HASHES_PER_CHUNK * 32);
    assert!(!dir.path().join("main-known-hashes-01.bin").exists());
    assert_eq!(
        spec.chunk_hashes[0],
        <[u8; 32]>::from(Sha256::digest(&chunk))
    );

    // One hash past the boundary spills into a single-hash second chunk.
    let dir = tempdir()?;
    let hashes = vec![[2u8; 32]; HASHES_PER_CHUNK + 1];
    let hints = vec![1u8; HASHES_PER_CHUNK + 1];

    let spec = emit_assets(dir.path(), "main", &hashes, &hints)?;

    assert_eq!(spec.chunk_hashes.len(), 2);
    let chunk = std::fs::read(dir.path().join("main-known-hashes-01.bin"))?;
    assert_eq!(chunk.len(), 32);
    assert_eq!(
        spec.chunk_hashes[1],
        <[u8; 32]>::from(Sha256::digest(&chunk))
    );

    let hint_file = std::fs::read(dir.path().join("main-size-hints.bin"))?;
    assert_eq!(hint_file, hints);
    assert_eq!(
        spec.size_hint_hash,
        <[u8; 32]>::from(Sha256::digest(&hint_file)),
    );

    Ok(())
}

#[test]
fn emit_assets_rejects_invalid_input() -> Result<()> {
    let dir = tempdir()?;

    // No genesis hash.
    assert!(emit_assets(dir.path(), "main", &[], &[]).is_err());

    // Hash and hint coverage must match.
    assert!(emit_assets(dir.path(), "main", &[[1; 32]; 2], &[1]).is_err());

    // Hint 0 marks an unswept height: the error must name it.
    let err = emit_assets(dir.path(), "main", &[[1; 32]; 2], &[1, 0]).unwrap_err();
    assert!(err.to_string().contains("height 1"), "{err}");

    Ok(())
}

#[test]
fn spec_constant_block_matches_synthetic_sweep() -> Result<()> {
    let dir = tempdir()?;
    let hashes = [[0xaa; 32], [0xbb; 32], [0xcc; 32]];
    let hints = [1u8, 254, 255];

    let spec = emit_assets(dir.path(), "test", &hashes, &hints)?;

    // The chunk digest covers the concatenated raw internal-order hashes.
    let concatenated: Vec<u8> = hashes.iter().flatten().copied().collect();
    let expected_chunk = <[u8; 32]>::from(Sha256::digest(&concatenated));
    let expected_hints = <[u8; 32]>::from(Sha256::digest(hints));

    assert_eq!(spec.chunk_hashes, vec![expected_chunk]);
    assert_eq!(spec.size_hint_hash, expected_hints);

    let constant_block = spec_constant_block("TESTNET", 2, &spec);

    assert!(
        constant_block.contains("pub const TESTNET_KNOWN_HASH_LIST_SPEC: KnownHashListSpec ="),
        "{constant_block}"
    );
    assert!(
        constant_block.contains("max_height: Height(2),"),
        "{constant_block}"
    );
    assert!(
        constant_block.contains(&format!("hex!(\"{}\"),", hex::encode(expected_chunk))),
        "{constant_block}"
    );
    assert!(
        constant_block.contains(&format!(
            "size_hint_hash: hex!(\"{}\"),",
            hex::encode(expected_hints),
        )),
        "{constant_block}"
    );

    Ok(())
}

#[test]
fn sweep_files_persist_sparse_entries() -> Result<()> {
    let dir = tempdir()?;
    let hash_path = dir.path().join("hashes.bin");
    let sizes_path = dir.path().join("sizes.u32le");

    {
        let mut files = SweepFiles::open(&hash_path, &sizes_path)?;
        assert!(files.hash(0).is_none());
        assert!(files.size(0).is_none());

        files.set_hash(3, block::Hash([3; 32]))?;
        files.set_size(3, 1_234)?;
        files.flush()?;
    }

    // Reopening sees the entry, and the sparse gap below it stays unfetched.
    let files = SweepFiles::open(&hash_path, &sizes_path)?;
    assert_eq!(files.hash(3), Some(block::Hash([3; 32])));
    assert_eq!(files.size(3), Some(1_234));
    assert!(files.hash(2).is_none());
    assert!(files.size(2).is_none());
    assert!(files.hash(4).is_none());

    // Completeness checks name the first missing height.
    let err = files.complete_hashes(3).unwrap_err();
    assert!(err.to_string().contains("height 0"), "{err}");
    let err = files.complete_sizes(3).unwrap_err();
    assert!(err.to_string().contains("height 0"), "{err}");

    Ok(())
}

#[tokio::test]
async fn sweep_fetches_verifies_and_resumes() -> Result<()> {
    let network = Network::Mainnet;
    let dir = tempdir()?;
    let hash_path = dir.path().join("hashes.bin");
    let sizes_path = dir.path().join("sizes.u32le");

    let chain = mock_mainnet_chain();
    let source = MockSource::new(chain.clone());

    let mut files = SweepFiles::open(&hash_path, &sizes_path)?;
    run_sweep(&source, &network, &mut files, 0, 4, 8).await?;

    // Each of the 5 heights needs one hash and one size fetch.
    assert_eq!(source.calls(), 10);
    for (height, (hash, size)) in &chain {
        assert_eq!(files.hash(*height), Some(*hash));
        assert_eq!(files.size(*height), Some(*size));
    }

    // The whole range is complete, so emission inputs are available.
    assert_eq!(files.complete_hashes(4)?.len(), 5);
    assert_eq!(files.complete_sizes(4)?.len(), 5);
    drop(files);

    // A resumed sweep finds nothing missing and makes no fetches.
    let mut files = SweepFiles::open(&hash_path, &sizes_path)?;
    run_sweep(&source, &network, &mut files, 0, 4, 8).await?;
    assert_eq!(source.calls(), 10);

    Ok(())
}

#[tokio::test]
async fn sweep_fails_on_checkpoint_anchor_mismatch() -> Result<()> {
    let network = Network::Mainnet;
    let dir = tempdir()?;
    let hash_path = dir.path().join("hashes.bin");
    let sizes_path = dir.path().join("sizes.u32le");

    // Height 0 is in the mainnet checkpoint list, and this hash is wrong.
    let mut chain = mock_mainnet_chain();
    chain.insert(0, (block::Hash([0xff; 32]), 1_687));
    let source = MockSource::new(chain);

    let mut files = SweepFiles::open(&hash_path, &sizes_path)?;
    let err = run_sweep(&source, &network, &mut files, 0, 4, 8)
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("anchor mismatch at height 0"),
        "{err}"
    );

    Ok(())
}
