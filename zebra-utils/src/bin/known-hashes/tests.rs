//! Tests for the known-hashes pinning tool, against a fixture node.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use color_eyre::eyre::{eyre, Result};
use tempfile::tempdir;

use zebra_chain::{
    block::{self, SyncHashEntry},
    parameters::{
        known_hashes::{chunk_pin, KnownHashError, KnownHashListSpec, CHUNK_HEIGHTS},
        Network, GENESIS_PREVIOUS_BLOCK_HASH,
    },
};

use crate::{
    emit::{chunk_pins, spec_constant_block},
    run_emit,
    source::{swept_block, BlockSource, SweptBlock},
    sweep::{run_sweep, SweepFile, RECORD_BYTES},
    tip_margin_warning, SERVED_TIP_MARGIN,
};

/// A [`BlockSource`] over a fixed in-memory chain, counting every fetch.
#[derive(Clone)]
struct FixtureNode {
    /// The fixture chain: the block at each height, from genesis.
    blocks: Arc<Vec<SweptBlock>>,

    /// The total number of entry fetches.
    calls: Arc<AtomicUsize>,
}

impl FixtureNode {
    fn new(blocks: Vec<SweptBlock>) -> Self {
        Self {
            blocks: Arc::new(blocks),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl BlockSource for FixtureNode {
    async fn tip_height(&self) -> Result<u32> {
        let len = u32::try_from(self.blocks.len())?;
        len.checked_sub(1)
            .ok_or_else(|| eyre!("the fixture chain is empty"))
    }

    async fn swept_block(&self, height: u32) -> Result<SweptBlock> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.blocks
            .get(usize::try_from(height)?)
            .copied()
            .ok_or_else(|| eyre!("no fixture block at height {height}"))
    }
}

/// A fixture Mainnet chain of `len` blocks: the real genesis block, the real
/// checkpoint hashes at checkpointed heights, and distinct fake hashes and
/// counts elsewhere, each block linked to the one below it.
fn fixture_mainnet_chain(len: u32) -> Vec<SweptBlock> {
    let checkpoints = Network::Mainnet.checkpoint_list();
    let genesis = swept_block(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES, 0)
        .expect("the genesis block is valid");

    let mut blocks = vec![genesis];
    for height in 1..len {
        let hash = checkpoints.hash(block::Height(height)).unwrap_or_else(|| {
            let mut hash = [0xAA; 32];
            hash[..4].copy_from_slice(&height.to_le_bytes());
            block::Hash(hash)
        });

        let previous_block_hash = blocks.last().expect("genesis is first").entry.hash;
        blocks.push(SweptBlock {
            entry: SyncHashEntry {
                hash,
                size: 1_000 + u64::from(height % 5_000),
                txs: 1 + u64::from(height % 7),
                txouts: 1 + u64::from(height % 11),
                notes: u64::from(height % 13),
            },
            previous_block_hash,
        });
    }

    blocks
}

/// The `get-hashes` entries of `chain`.
fn entries(chain: &[SweptBlock]) -> Vec<SyncHashEntry> {
    chain.iter().map(|swept| swept.entry).collect()
}

/// The tool derives each entry and parent hash from the raw block, with the
/// same counts the state serves, and checks the block's height.
#[test]
fn swept_block_from_raw_block() -> Result<()> {
    let _init_guard = zebra_test::init();

    let genesis = swept_block(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES, 0)?;
    assert_eq!(genesis.entry.hash, Network::Mainnet.genesis_hash());
    assert_eq!(genesis.previous_block_hash, GENESIS_PREVIOUS_BLOCK_HASH);
    assert_eq!(genesis.entry.size, 1_692);
    assert_eq!(
        (genesis.entry.txs, genesis.entry.txouts, genesis.entry.notes),
        (1, 1, 0)
    );

    let bytes = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.as_slice();
    let block_1 = swept_block(bytes, 1)?;
    assert_eq!(block_1.previous_block_hash, genesis.entry.hash);
    assert_eq!(block_1.entry.size, bytes.len() as u64);
    assert_eq!(block_1.entry.txs, 1);
    assert_eq!(block_1.entry.notes, 0);

    // A node that answers a height with the block at another height fails.
    let err = swept_block(bytes, 2).unwrap_err();
    assert!(err.to_string().contains("for height 2"), "{err}");

    // Truncated bytes are not a block.
    assert!(swept_block(&bytes[..bytes.len() - 1], 1).is_err());

    Ok(())
}

/// Sweep records persist sparsely, and reopening sees exactly what was
/// written.
#[test]
fn sweep_file_persists_sparse_records() -> Result<()> {
    let _init_guard = zebra_test::init();

    let dir = tempdir()?;
    let path = dir.path().join("sweep.bin");
    let entry = SweptBlock {
        entry: SyncHashEntry {
            hash: block::Hash([3; 32]),
            size: u64::MAX,
            txs: 2,
            txouts: 0x0102_0304_0506_0708,
            notes: 9,
        },
        previous_block_hash: block::Hash([4; 32]),
    };

    {
        let mut file = SweepFile::open(&path)?;
        assert!(file.entry(0).is_none());

        file.set_entry(3, entry)?;
        file.flush()?;

        // An all-zero hash is reserved for unfetched records.
        let mut unfetched = entry;
        unfetched.entry.hash = block::Hash([0; 32]);
        assert!(file.set_entry(1, unfetched).is_err());
    }

    assert_eq!(std::fs::metadata(&path)?.len(), 4 * RECORD_BYTES as u64);

    let file = SweepFile::open(&path)?;
    assert_eq!(file.entry(3), Some(entry));
    assert!(file.entry(2).is_none());
    assert!(file.entry(4).is_none());

    let err = file.complete_entries(3).unwrap_err();
    assert!(err.to_string().contains("height 0"), "{err}");
    let err = file.complete_entries(4).unwrap_err();
    assert!(
        err.to_string().contains("only covers heights below 4"),
        "{err}"
    );

    // A truncated record is corruption.
    std::fs::write(&path, [0; RECORD_BYTES + 1])?;
    assert!(SweepFile::open(&path).is_err());

    Ok(())
}

#[tokio::test]
async fn sweep_fetches_verifies_and_resumes() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let dir = tempdir()?;
    let path = dir.path().join("sweep.bin");

    let chain = fixture_mainnet_chain(5);
    let node = FixtureNode::new(chain.clone());

    let mut file = SweepFile::open(&path)?;
    run_sweep(&node, &network, &mut file, 0, 4, 8).await?;

    assert_eq!(node.calls(), 5);
    assert_eq!(file.complete_entries(4)?, entries(&chain));
    drop(file);

    // A resumed sweep finds nothing missing and makes no fetches.
    let mut file = SweepFile::open(&path)?;
    run_sweep(&node, &network, &mut file, 0, 4, 8).await?;
    assert_eq!(node.calls(), 5);

    Ok(())
}

#[tokio::test]
async fn sweep_fails_on_checkpoint_anchor_mismatch() -> Result<()> {
    let _init_guard = zebra_test::init();

    let dir = tempdir()?;
    let mut file = SweepFile::open(&dir.path().join("sweep.bin"))?;

    // Height 0 is in the Mainnet checkpoint list, and this hash is wrong.
    let mut chain = fixture_mainnet_chain(5);
    chain[0].entry.hash = block::Hash([0xff; 32]);
    let node = FixtureNode::new(chain);

    let err = run_sweep(&node, &Network::Mainnet, &mut file, 0, 4, 8)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("anchor mismatch at height 0"),
        "{err}"
    );

    Ok(())
}

/// End to end against the fixture node: the sweep, then the pins, which a
/// node accepts for the fixture chain's chunks and rejects for any changed
/// entry.
#[tokio::test]
async fn pins_from_fixture_node_verify() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let dir = tempdir()?;

    // Two full chunks and a three-height tail.
    let max_height = 2 * CHUNK_HEIGHTS + 2;
    let swept = fixture_mainnet_chain(max_height + 1);
    let node = FixtureNode::new(swept.clone());
    let chain = entries(&swept);

    let mut file = SweepFile::open(&dir.path().join("sweep.bin"))?;
    run_sweep(&node, &network, &mut file, 0, max_height, 64).await?;

    let block = run_emit(&network, &file, max_height)?;

    let pins = chunk_pins(&network, &chain)?;
    assert_eq!(pins.len(), 3);
    assert_eq!(block, spec_constant_block("MAINNET", max_height, &pins));
    for (index, chunk) in chain.chunks(CHUNK_HEIGHTS as usize).enumerate() {
        assert_eq!(pins[index], chunk_pin(chunk));
        assert!(block.contains(&format!("// {index}: {}", hex::encode(pins[index]))));
    }

    let spec = KnownHashListSpec {
        max_height: block::Height(max_height),
        chunk_heights: CHUNK_HEIGHTS,
        chunk_hashes: pins.clone().into(),
    };
    assert_eq!(spec.chunk_len(2), Some(3));
    for (index, chunk) in chain.chunks(CHUNK_HEIGHTS as usize).enumerate() {
        assert_eq!(spec.verify_chunk(&network, index, chunk), Ok(()));
    }

    // A node serving one different count produces a different pin.
    let mut lying = chain[CHUNK_HEIGHTS as usize..2 * CHUNK_HEIGHTS as usize].to_vec();
    lying[10].notes += 1;
    assert_eq!(
        spec.verify_chunk(&network, 1, &lying),
        Err(KnownHashError::ChunkHashMismatch { index: 1 }),
    );

    Ok(())
}

/// Pinning refuses incomplete sweeps, lists past the final checkpoint, and
/// sweeps of another network.
#[tokio::test]
async fn emit_refuses_bad_sweeps() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let dir = tempdir()?;
    let mut file = SweepFile::open(&dir.path().join("sweep.bin"))?;

    let node = FixtureNode::new(fixture_mainnet_chain(10));
    run_sweep(&node, &network, &mut file, 0, 9, 4).await?;
    run_emit(&network, &file, 9)?;

    let err = run_emit(&network, &file, 10).unwrap_err();
    assert!(
        err.to_string().contains("only covers heights below 10"),
        "{err}"
    );

    let final_checkpoint = network.checkpoint_list().max_height().0;
    let err = run_emit(&network, &file, final_checkpoint + 1).unwrap_err();
    assert!(err.to_string().contains("final checkpoint"), "{err}");

    let testnet = Network::new_default_testnet();
    assert!(chunk_pins(&testnet, &file.complete_entries(9)?).is_err());

    Ok(())
}

/// A block between two anchors that is not the child of the block below it
/// fails the sweep and the pinning, even though every anchor matches.
#[tokio::test]
async fn broken_parent_link_between_anchors_fails() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let checkpoints = network.checkpoint_list();
    let dir = tempdir()?;
    let mut file = SweepFile::open(&dir.path().join("sweep.bin"))?;

    // The fixture must reach the second anchor, so the broken height is
    // strictly between two anchors.
    let (anchor, _) = checkpoints
        .iter_cloned()
        .nth(1)
        .expect("the Mainnet checkpoint list has at least two checkpoints");
    let len = anchor.0 + 1;
    let broken = anchor.0 / 2;
    assert!(checkpoints.hash(block::Height(broken)).is_none());

    // The node serves a block from another chain at `broken`: its own hash
    // looks plausible, but its parent is not the swept block below it.
    let mut chain = fixture_mainnet_chain(len);
    chain[broken as usize].previous_block_hash = block::Hash([0xBB; 32]);
    let node = FixtureNode::new(chain);

    let err = run_sweep(&node, &network, &mut file, 0, len - 1, 16)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("link mismatch at height {broken}")),
        "{err}"
    );

    // Every fetched block was recorded before the final check, so the sweep
    // file is complete, and pinning it must fail the same way.
    let err = run_emit(&network, &file, len - 1).unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("link mismatch at height {broken}")),
        "{err}"
    );

    Ok(())
}

/// A genesis block with a parent fails the link check.
#[tokio::test]
async fn genesis_with_parent_fails() -> Result<()> {
    let _init_guard = zebra_test::init();

    let dir = tempdir()?;
    let mut file = SweepFile::open(&dir.path().join("sweep.bin"))?;

    let mut chain = fixture_mainnet_chain(3);
    chain[0].previous_block_hash = block::Hash([0xBB; 32]);
    let node = FixtureNode::new(chain);

    let err = run_sweep(&node, &Network::Mainnet, &mut file, 0, 2, 4)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("link mismatch at height 0"),
        "{err}"
    );

    Ok(())
}

/// The printed constant block matches the `KnownHashListSpec` fields, so it
/// can be pasted into `zebra-chain`.
#[test]
fn spec_constant_block_format() {
    let _init_guard = zebra_test::init();

    let mut pin = [0; 32];
    pin[0] = 0xab;
    pin[31] = 0x01;

    let block = spec_constant_block("TESTNET", 7, &[pin]);

    let mut bytes = vec!["0x00"; 32];
    bytes[0] = "0xab";
    bytes[31] = "0x01";

    let expected = format!(
        "/// The TESTNET known-hash list: 8 heights (0..=7) in 1 chunks,\n\
         /// generated by the `known-hashes` tool from a checkpoint-anchored sweep.\n\
         pub const TESTNET_KNOWN_HASHES: KnownHashListSpec = KnownHashListSpec {{\n\
         \x20   max_height: block::Height(7),\n\
         \x20   chunk_heights: CHUNK_HEIGHTS,\n\
         \x20   chunk_hashes: Cow::Borrowed(&[\n\
         \x20       // 0: {}\n\
         \x20       [{}],\n\
         \x20   ]),\n\
         }};\n",
        hex::encode(pin),
        bytes.join(", "),
    );
    assert_eq!(block, expected);
}

/// Emitting a list that ends within the served margin of the node tip warns,
/// because no node can serve its last chunk yet.
#[test]
fn emit_warns_near_the_tip() {
    let _init_guard = zebra_test::init();

    assert_eq!(tip_margin_warning(1_000, 2_000), None);
    assert_eq!(tip_margin_warning(0, SERVED_TIP_MARGIN), None);
    assert!(tip_margin_warning(1_001, 2_000).is_some());
    assert!(tip_margin_warning(0, SERVED_TIP_MARGIN - 1).is_some());
    assert!(tip_margin_warning(5, 0).is_some());
}
