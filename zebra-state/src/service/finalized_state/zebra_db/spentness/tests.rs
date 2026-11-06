//! Compare hinted construction with ordinary state, and exercise its recovery.
//!
//! Ported from `zakura/feat/spentness-writer`
//! (`crates/zakura-state/src/service/finalized_state/tests/spentness.rs`), plus the
//! differential pattern of `ibd-utxo-write-elision`'s `elision_matches_no_elision`:
//! a hinted and an unhinted sync of the same chain give byte-identical column
//! families.

use std::{collections::BTreeMap, sync::Arc};

use sha2::{Digest, Sha256};
use tokio::sync::watch;

use zebra_chain::{
    amount::{Amount, DeferredPoolBalanceChange},
    block::{Block, Height},
    parameters::{
        spentness_hints::{ArtifactEncoder, MaxCheckpoint, ParsedArtifact, VerifiedArtifact},
        Network, NetworkKind,
    },
    serialization::ZcashDeserializeInto,
    transaction::{LockTime, Transaction},
    transparent::{Address, Input, OutPoint, Output, Script},
};

use super::{
    read_progress,
    record::{Phase, Progress},
    SpentnessError, SpentnessHintsReceiver, SpentnessWriter, SPENTNESS_ARTIFACT, SPENTNESS_JOURNAL,
};
use crate::{
    request::{FinalizedBlock, Treestate},
    service::finalized_state::{
        disk_db::DiskWriteBatch,
        disk_format::{IntoDisk, OutputLocation, RawBytes, TransactionLocation},
        FinalizedState, STATE_COLUMN_FAMILIES_IN_CODE,
    },
    CheckpointVerifiedBlock, Config,
};

/// A chain, its ordinary (unhinted) finalized state, and a hint artifact for it.
struct Fixture {
    _directory: tempfile::TempDir,
    config: Config,
    network: Network,
    blocks: Vec<Arc<Block>>,
    ordinary: FinalizedState,
    artifact: Arc<VerifiedArtifact>,
}

impl Fixture {
    /// Mainnet blocks 0..=10, which only have coinbase outputs.
    fn mainnet(flipped: Option<OutputLocation>) -> Self {
        let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .iter()
            .filter(|(height, _)| **height <= 10)
            .map(|(_, bytes)| Arc::new(bytes.zcash_deserialize_into::<Block>().unwrap()))
            .collect();
        assert_eq!(blocks.len(), 11);
        Self::from_blocks(blocks, Network::Mainnet, flipped)
    }

    /// The generated transparent chain, see [`generated_transparent_chain`].
    fn generated(flipped: Option<OutputLocation>) -> Self {
        let (blocks, network) = generated_transparent_chain();
        Self::from_blocks(blocks, network, flipped)
    }

    /// Builds the ordinary state, and an artifact whose bit at `flipped` is wrong, if set.
    fn from_blocks(
        blocks: Vec<Arc<Block>>,
        network: Network,
        flipped: Option<OutputLocation>,
    ) -> Self {
        let mut ordinary = open(&Config::ephemeral(), &network);
        for block in &blocks {
            ordinary
                .commit_finalized_direct(
                    CheckpointVerifiedBlock::from(block.clone()).into(),
                    None,
                    "spentness oracle",
                )
                .unwrap();
        }

        let artifact = artifact(&blocks, &network, |location| {
            let survives = ordinary.db.utxo_by_location(location).is_some();
            survives != (flipped == Some(location))
        });

        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            cache_dir: directory.path().to_path_buf(),
            ..Config::default()
        };

        Self {
            _directory: directory,
            config,
            network,
            blocks,
            ordinary,
            artifact,
        }
    }

    /// Opens the hinted state, and its writer with the fixture artifact.
    fn open(&self) -> (FinalizedState, Option<SpentnessWriter>) {
        self.open_with(hints(Some(self.artifact.clone())))
    }

    fn open_with(
        &self,
        hints: SpentnessHintsReceiver,
    ) -> (FinalizedState, Option<SpentnessWriter>) {
        self.open_with_max_checkpoint(hints, Height::MAX)
    }

    fn open_with_max_checkpoint(
        &self,
        hints: SpentnessHintsReceiver,
        max_checkpoint_height: Height,
    ) -> (FinalizedState, Option<SpentnessWriter>) {
        let state = open(&self.config, &self.network);
        let writer = SpentnessWriter::new(&state.db, hints, max_checkpoint_height);
        (state, writer)
    }

    fn terminal_height(&self) -> Height {
        Height((self.blocks.len() - 1).try_into().unwrap())
    }

    /// Commits the fixture blocks at `heights` with hints, failing on the first error.
    fn commit(
        &self,
        state: &mut FinalizedState,
        writer: &mut Option<SpentnessWriter>,
        heights: std::ops::RangeInclusive<usize>,
    ) {
        for height in heights {
            commit(state, writer, &self.blocks[height]).unwrap();
        }
    }

    /// Commits blocks until one fails, and returns its spentness failure.
    fn first_failure(
        &self,
        state: &mut FinalizedState,
        writer: &mut Option<SpentnessWriter>,
    ) -> SpentnessError {
        for block in &self.blocks {
            if let Err(error) = commit(state, writer, block) {
                return error
                    .spentness_failure()
                    .expect("hinted construction reports its own failures")
                    .clone();
            }
        }
        panic!("a false artifact must never complete construction");
    }
}

fn open(config: &Config, network: &Network) -> FinalizedState {
    FinalizedState::new(
        config,
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .unwrap()
}

fn hints(artifact: Option<Arc<VerifiedArtifact>>) -> SpentnessHintsReceiver {
    watch::channel(artifact).1
}

fn commit(
    state: &mut FinalizedState,
    writer: &mut Option<SpentnessWriter>,
    block: &Arc<Block>,
) -> Result<(), crate::error::CommitCheckpointVerifiedError> {
    state
        .commit_finalized_direct_with_spentness(
            CheckpointVerifiedBlock::from(block.clone()).into(),
            None,
            "spentness construction",
            writer.as_mut(),
        )
        .map(|_| ())
}

/// Every output location of `blocks`, in artifact order.
fn locations(blocks: &[Arc<Block>]) -> impl Iterator<Item = OutputLocation> + '_ {
    blocks.iter().enumerate().flat_map(|(height, block)| {
        block
            .transactions
            .iter()
            .enumerate()
            .flat_map(move |(tx_index, transaction)| {
                (0..transaction.outputs().len()).map(move |output_index| {
                    OutputLocation::from_usize(
                        Height(height.try_into().unwrap()),
                        tx_index,
                        output_index,
                    )
                })
            })
    })
}

/// An artifact for `blocks`, verified against a pin made for it.
fn artifact(
    blocks: &[Arc<Block>],
    network: &Network,
    retained: impl Fn(OutputLocation) -> bool,
) -> Arc<VerifiedArtifact> {
    let terminal = blocks.last().unwrap();
    let terminal_height = Height((blocks.len() - 1).try_into().unwrap());
    let output_count = locations(blocks).count().try_into().unwrap();
    let mut encoder = ArtifactEncoder::new(
        blocks[0].hash(),
        terminal_height,
        terminal.hash(),
        output_count,
    )
    .unwrap();
    for location in locations(blocks) {
        encoder.push(retained(location)).unwrap();
    }
    let bytes: Arc<[u8]> = encoder.finish().unwrap().into();
    let pin = MaxCheckpoint {
        height: terminal_height,
        hash: terminal.hash(),
        spentness_hash: Sha256::digest(&bytes).into(),
    };

    Arc::new(
        ParsedArtifact::parse(bytes)
            .unwrap()
            .verify(network, &pin)
            .unwrap(),
    )
}

fn entries(state: &FinalizedState, name: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let cf = state.db.db.cf_handle(name).unwrap();
    state
        .db
        .db
        .zs_forward_range_iter::<_, RawBytes, RawBytes, _>(&cf, ..)
        .map(|(key, value)| (key.raw_bytes().clone(), value.raw_bytes().clone()))
        .collect()
}

/// Every column family must be byte-identical, except the construction record.
fn assert_matches_ordinary(state: &FinalizedState, ordinary: &FinalizedState) {
    for name in STATE_COLUMN_FAMILIES_IN_CODE {
        if *name == SPENTNESS_ARTIFACT {
            continue;
        }
        assert_eq!(entries(state, name), entries(ordinary, name), "{name}");
    }
}

fn has_incomplete_hints(state: &FinalizedState) -> bool {
    super::has_incomplete_spentness_hints(&state.db)
}

fn progress(state: &FinalizedState) -> Option<Progress> {
    read_progress(&state.db)
        .unwrap()
        .map(|(_, progress)| progress)
}

/// Hinted construction writes the same state as ordinary sync, across a restart.
///
/// The restart empties the live map, and block 102's 70 outputs overflow the test
/// budget, so later spends resolve from the journal.
#[test]
fn hinted_sync_matches_ordinary_state_across_a_restart() {
    let _init_guard = zebra_test::init();
    let mut fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=100);
    assert!(matches!(
        progress(&state),
        Some(Progress {
            phase: Phase::Applying,
            height: Height(100),
            ..
        })
    ));
    assert!(!entries(&state, SPENTNESS_JOURNAL).is_empty());
    drop((state, writer));

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 101..=GENERATED_TIP as usize);
    let complete = progress(&state).unwrap();
    assert_eq!(complete.phase, Phase::Complete);
    assert_eq!(complete.omitted, complete.resolved);
    assert!(complete.omitted > 2 * u64::from(FAN_OUT));
    assert!(entries(&state, SPENTNESS_JOURNAL).is_empty());
    assert_matches_ordinary(&state, &fixture.ordinary);

    // Ordinary commits continue above H.
    assert!(!writer
        .as_ref()
        .unwrap()
        .applies_to(Height(GENERATED_TIP + 1)));
    let next = synthetic_block(
        fixture.blocks.last().unwrap(),
        GENERATED_TIP + 1,
        Vec::new(),
    );
    commit(&mut state, &mut writer, &next).unwrap();
    commit(&mut fixture.ordinary, &mut None, &next).unwrap();
    assert_matches_ordinary(&state, &fixture.ordinary);

    // A completed state needs no artifact.
    drop((state, writer));
    let (_state, writer) = fixture.open_with(hints(None));
    assert!(writer.is_none());
}

/// Mainnet blocks resume after a restart and complete at H.
#[test]
fn hinted_sync_matches_mainnet_blocks_and_resumes() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::mainnet(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=4);
    drop((state, writer));

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 5..=10);
    assert!(matches!(
        progress(&state),
        Some(Progress {
            phase: Phase::Complete,
            height: Height(10),
            ..
        })
    ));
    assert_matches_ordinary(&state, &fixture.ordinary);
}

#[test]
fn hinted_sync_rejects_an_omitted_survivor_at_h() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::mainnet(Some(OutputLocation::from_usize(Height(1), 0, 0)));

    let (mut state, mut writer) = fixture.open();
    let error = fixture.first_failure(&mut state, &mut writer);
    assert!(
        matches!(error, SpentnessError::UnbalancedAtTerminal { .. }),
        "{error}"
    );
    assert_eq!(state.db.finalized_tip_height(), Some(Height(9)));
}

#[test]
fn hinted_sync_rejects_a_zero_value_non_address_omission() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(Some(OutputLocation::from_usize(Height(1), 0, 1)));

    let (mut state, mut writer) = fixture.open();
    let error = fixture.first_failure(&mut state, &mut writer);
    assert!(
        matches!(error, SpentnessError::UnbalancedAtTerminal { .. }),
        "{error}"
    );
    assert_eq!(
        state.db.finalized_tip_height(),
        Some(Height(GENERATED_TIP - 1))
    );
}

#[test]
fn hinted_sync_rejects_a_retained_spent_output() {
    let _init_guard = zebra_test::init();
    // Block 101 spends this coinbase output, but the artifact retains it.
    let fixture = Fixture::generated(Some(OutputLocation::from_usize(Height(1), 0, 0)));

    let (mut state, mut writer) = fixture.open();
    let error = fixture.first_failure(&mut state, &mut writer);
    assert_eq!(
        error,
        SpentnessError::UnresolvedSpend {
            height: Height(101)
        }
    );
    assert!(error.to_string().contains("delete the state"), "{error}");
    assert_eq!(state.db.finalized_tip_height(), Some(Height(100)));
}

#[test]
fn hinted_sync_rejects_a_same_block_spend_of_a_survivor() {
    let _init_guard = zebra_test::init();
    // Block 101's first transaction is spent by its second one, but the artifact retains it.
    let fixture = Fixture::generated(Some(OutputLocation::from_usize(Height(101), 1, 0)));

    let (mut state, mut writer) = fixture.open();
    let error = fixture.first_failure(&mut state, &mut writer);
    assert_eq!(
        error,
        SpentnessError::UnresolvedSpend {
            height: Height(101)
        }
    );
}

#[test]
fn hinted_sync_rejects_a_shifted_cursor() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=50);
    let (commitment, mut progress) = read_progress(&state.db).unwrap().unwrap();
    progress.next_ordinal += 1;
    let mut batch = DiskWriteBatch::new();
    batch.prepare_spentness_progress(&state.db, commitment, progress);
    state.db.write_batch(batch).unwrap();
    drop((state, writer));

    let (mut state, mut writer) = fixture.open();
    let failed = fixture.blocks[51..]
        .iter()
        .find_map(|block| commit(&mut state, &mut writer, block).err());
    let failure = failed.expect("a shifted cursor must never complete construction");
    assert!(failure.spentness_failure().is_some(), "{failure:?}");
    assert_ne!(
        state.db.finalized_tip_height(),
        Some(fixture.terminal_height())
    );
}

/// A block whose batch was prepared but not written can be committed again.
#[test]
fn hinted_sync_retries_an_unwritten_batch() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=102);

    // Apply block 103, which spends block 102's fan-out, then drop its batch.
    let block = fixture.blocks[103].clone();
    let finalized = FinalizedBlock::from_checkpoint_verified(
        CheckpointVerifiedBlock::from(block.clone()),
        Treestate::default(),
        DeferredPoolBalanceChange::zero(),
    );
    let new_outputs: BTreeMap<_, _> = finalized
        .new_outputs
        .iter()
        .map(|(outpoint, utxo)| {
            let tx_index = finalized
                .transaction_hashes
                .iter()
                .position(|hash| *hash == outpoint.hash)
                .unwrap();
            (
                OutputLocation::from_outpoint(
                    TransactionLocation::from_usize(Height(103), tx_index),
                    outpoint,
                ),
                utxo.utxo.clone(),
            )
        })
        .collect();
    let hinted = writer
        .as_mut()
        .unwrap()
        .apply_block(
            &state.db,
            &finalized,
            &new_outputs,
            &mut DiskWriteBatch::new(),
        )
        .unwrap();
    assert_eq!(hinted.spent.len(), usize::from(FAN_OUT));
    drop(hinted);

    fixture.commit(&mut state, &mut writer, 103..=GENERATED_TIP as usize);
    assert_eq!(progress(&state).unwrap().phase, Phase::Complete);
    assert_matches_ordinary(&state, &fixture.ordinary);
}

/// Without an artifact, the writer behaves exactly like an unhinted sync.
#[test]
fn no_artifact_gives_an_ordinary_sync() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open_with(hints(None));
    assert!(writer.is_some());
    fixture.commit(&mut state, &mut writer, 0..=GENERATED_TIP as usize);
    assert_eq!(progress(&state), None);
    assert!(entries(&state, SPENTNESS_JOURNAL).is_empty());
    assert_matches_ordinary(&state, &fixture.ordinary);

    // A non-empty state without a record never adopts hints.
    drop((state, writer));
    let (_state, writer) = fixture.open();
    assert!(writer.is_none());
}

/// An artifact that arrives after genesis is adopted for block 1. (A state that
/// committed block 1 without hints never adopts them: see
/// [`no_artifact_gives_an_ordinary_sync`].)
#[test]
fn artifact_is_adopted_only_while_the_tip_is_at_most_genesis() {
    let _init_guard = zebra_test::init();

    let fixture = Fixture::generated(None);
    let (sender, receiver) = watch::channel(None);
    let (mut state, mut writer) = fixture.open_with(receiver);
    fixture.commit(&mut state, &mut writer, 0..=0);

    sender.send_replace(Some(fixture.artifact.clone()));
    fixture.commit(&mut state, &mut writer, 1..=GENERATED_TIP as usize);

    assert_eq!(progress(&state).unwrap().phase, Phase::Complete);
    assert_matches_ordinary(&state, &fixture.ordinary);
}

/// Block 1 waits for the node's hints decision, whichever path submits it (F-345).
#[test]
fn block_one_waits_for_the_hints_decision() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (sender, receiver) = watch::channel(None);
    let (mut state, mut writer) = fixture.open_with(receiver);
    fixture.commit(&mut state, &mut writer, 0..=0);

    let artifact = fixture.artifact.clone();
    let delivery = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        sender.send_replace(Some(artifact));
        sender
    });
    // Block 1 arrives before the artifact, as from `submit_block` or gossip.
    fixture.commit(&mut state, &mut writer, 1..=GENERATED_TIP as usize);
    let _sender = delivery.join().unwrap();

    assert_eq!(progress(&state).unwrap().phase, Phase::Complete);
    assert_matches_ordinary(&state, &fixture.ordinary);
}

/// Once the node decides on no hints by dropping its sender, block 1 commits without hints.
#[test]
fn block_one_commits_without_hints_after_the_sender_drops() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (sender, receiver) = watch::channel(None);
    let (mut state, mut writer) = fixture.open_with(receiver);
    fixture.commit(&mut state, &mut writer, 0..=0);

    let dropper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(sender);
    });
    fixture.commit(&mut state, &mut writer, 1..=GENERATED_TIP as usize);
    dropper.join().unwrap();

    assert_eq!(progress(&state), None);
    assert!(entries(&state, SPENTNESS_JOURNAL).is_empty());
    assert_matches_ordinary(&state, &fixture.ordinary);
}

/// An incomplete hinted state refuses to continue without its artifact.
#[test]
fn resume_requires_the_same_artifact() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=10);
    drop((state, writer));

    let other = Fixture::mainnet(None).artifact;
    for hints in [hints(None), hints(Some(other))] {
        let (mut state, mut writer) = fixture.open_with(hints);
        let error = commit(&mut state, &mut writer, &fixture.blocks[11]).unwrap_err();
        assert_eq!(
            error.spentness_failure(),
            Some(&SpentnessError::MissingArtifact)
        );
        assert_eq!(state.db.finalized_tip_height(), Some(Height(10)));
    }
}

/// A resumed writer waits for an artifact that arrives after its first block is queued.
#[test]
fn resume_waits_for_a_late_artifact() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=10);
    drop((state, writer));

    let (sender, receiver) = watch::channel(None);
    let (mut state, mut writer) = fixture.open_with(receiver);
    let artifact = fixture.artifact.clone();
    let delivery = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        sender.send_replace(Some(artifact));
        sender
    });
    fixture.commit(&mut state, &mut writer, 11..=GENERATED_TIP as usize);
    let _sender = delivery.join().unwrap();

    assert_eq!(progress(&state).unwrap().phase, Phase::Complete);
    assert_matches_ordinary(&state, &fixture.ordinary);
}

/// A resumed writer waits until the node gives up on the artifact, then fails.
#[test]
fn resume_fails_once_the_sender_drops_without_the_artifact() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=10);
    drop((state, writer));

    let (sender, receiver) = watch::channel(None);
    let (mut state, mut writer) = fixture.open_with(receiver);
    assert!(has_incomplete_hints(&state));
    let dropper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(sender);
    });
    let error = commit(&mut state, &mut writer, &fixture.blocks[11]).unwrap_err();
    dropper.join().unwrap();
    assert_eq!(
        error.spentness_failure(),
        Some(&SpentnessError::MissingArtifact)
    );
    assert_eq!(state.db.finalized_tip_height(), Some(Height(10)));
}

/// The state service waits for the artifact of an incomplete hinted sync, which
/// the node sends after network init, while the first resumed block is queued.
#[tokio::test(flavor = "multi_thread")]
async fn state_service_resumes_with_a_late_artifact() {
    use tower::{Service, ServiceExt};

    use crate::{Request, Response};

    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=10);
    drop((state, writer));

    let (sender, receiver) = watch::channel(None);
    let (mut state, read_state, _, _) = crate::init_with_spentness(
        fixture.config.clone(),
        &fixture.network,
        Height::MAX,
        0,
        receiver,
    )
    .await;
    assert!(read_state.has_incomplete_spentness_hints());

    let first = state
        .ready()
        .await
        .unwrap()
        .call(Request::CommitCheckpointVerifiedBlock(
            fixture.blocks[11].clone().into(),
        ));
    let first = tokio::spawn(first);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!first.is_finished());
    sender.send_replace(Some(fixture.artifact.clone()));
    assert!(matches!(first.await.unwrap(), Ok(Response::Committed(_))));

    for block in &fixture.blocks[12..] {
        let response = state
            .ready()
            .await
            .unwrap()
            .call(Request::CommitCheckpointVerifiedBlock(block.clone().into()))
            .await;
        assert!(matches!(response, Ok(Response::Committed(_))));
    }
    assert!(!read_state.has_incomplete_spentness_hints());
    assert_eq!(
        read_progress(read_state.db()).unwrap().unwrap().1.phase,
        Phase::Complete
    );
}

/// An artifact whose `H` is above the max checkpoint height is ignored, because the
/// blocks above that height are semantically verified against the UTXO set.
#[test]
fn artifact_above_the_max_checkpoint_is_not_adopted() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);
    let below_h = (fixture.terminal_height() - 1).unwrap();

    let (mut state, mut writer) =
        fixture.open_with_max_checkpoint(hints(Some(fixture.artifact.clone())), below_h);
    assert!(!writer.as_ref().unwrap().applies_to(Height(0)));
    assert!(!writer.as_ref().unwrap().applies_to(Height(1)));
    fixture.commit(&mut state, &mut writer, 0..=GENERATED_TIP as usize);
    assert_eq!(progress(&state), None);
    assert!(entries(&state, SPENTNESS_JOURNAL).is_empty());
    assert_matches_ordinary(&state, &fixture.ordinary);
}

/// An incomplete hinted sync refuses to continue under a max checkpoint height below `H`.
#[test]
fn resume_rejects_an_artifact_above_the_max_checkpoint() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=10);
    drop((state, writer));

    let max_checkpoint_height = (fixture.terminal_height() - 1).unwrap();
    let (mut state, mut writer) = fixture
        .open_with_max_checkpoint(hints(Some(fixture.artifact.clone())), max_checkpoint_height);
    let error = commit(&mut state, &mut writer, &fixture.blocks[11]).unwrap_err();
    assert_eq!(
        error.spentness_failure(),
        Some(&SpentnessError::TerminalAboveMaxCheckpoint {
            terminal_height: fixture.terminal_height(),
            max_checkpoint_height,
        })
    );
    assert_eq!(state.db.finalized_tip_height(), Some(Height(10)));
}

/// A hinted sync on a network with NU7 and an NSM seed configured just above H
/// matches the ordinary state.
///
/// Zebra seeds the NSM balance in the NU7 activation block, which needs NU5 block
/// commitments that synthetic blocks lack, so the seed block itself is not committed:
/// zakura's "seed at H" case does not exist in Zebra.
#[test]
fn hinted_sync_matches_the_ordinary_nsm_seed() {
    let _init_guard = zebra_test::init();

    for nu7 in [GENERATED_TIP + 1, GENERATED_TIP + 2] {
        let (blocks, _) = generated_transparent_chain();
        let network = generated_network(&blocks[0], Some(nu7));
        let fixture = Fixture::from_blocks(blocks, network, None);

        let (mut state, mut writer) = fixture.open();
        fixture.commit(&mut state, &mut writer, 0..=GENERATED_TIP as usize);
        assert_eq!(progress(&state).unwrap().phase, Phase::Complete, "{nu7}");
        assert_matches_ordinary(&state, &fixture.ordinary);
    }
}

fn synthetic_block(
    previous: &Block,
    height: u32,
    transactions: Vec<Arc<Transaction>>,
) -> Arc<Block> {
    let script = Address::from_pub_key_hash(NetworkKind::Testnet, [7; 20]).script();
    let coinbase = Arc::new(Transaction::test_v1(
        vec![Input::Coinbase {
            height: Height(height),
            data: vec![0, 0],
            sequence: u32::MAX,
        }],
        vec![
            Output::new(Amount::try_from(100u64).unwrap(), script),
            Output::new(Amount::zero(), Script::new(&[0x6a])),
            Output::new(Amount::try_from(5u64).unwrap(), Script::new(&[0x51])),
        ],
        LockTime::Height(Height(0)),
    ));
    let mut block = previous.clone();
    block.transactions = std::iter::once(coinbase).chain(transactions).collect();
    let header = Arc::make_mut(&mut block.header);
    header.previous_block_hash = previous.hash();
    header.merkle_root = block.transactions.iter().collect();
    Arc::new(block)
}

fn spending_transaction(outpoint: OutPoint, output: Output) -> Arc<Transaction> {
    transparent_transaction(vec![outpoint], vec![output])
}

fn transparent_transaction(outpoints: Vec<OutPoint>, outputs: Vec<Output>) -> Arc<Transaction> {
    Arc::new(Transaction::test_v1(
        outpoints
            .into_iter()
            .map(|outpoint| Input::PrevOut {
                outpoint,
                unlock_script: Script::new(&[]),
                sequence: u32::MAX,
            })
            .collect(),
        outputs,
        LockTime::Height(Height(0)),
    ))
}

/// A one-zatoshi output to a distinct address for each `index`.
fn indexed_output(index: u8) -> Output {
    Output::new(
        Amount::try_from(1u64).unwrap(),
        Address::from_pub_key_hash(NetworkKind::Testnet, [index; 20]).script(),
    )
}

/// Outputs that one transaction pays to, and that distinct transactions spend.
const FAN_OUT: u8 = 70;

/// The terminal height of the generated chain.
const GENERATED_TIP: u32 = 104;

/// A transparent chain through [`GENERATED_TIP`] with same-block, cross-restart,
/// evicted, and many-creator spends.
fn generated_transparent_chain() -> (Vec<Arc<Block>>, Network) {
    let genesis: Arc<Block> = Arc::new(
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]
            .zcash_deserialize_into()
            .unwrap(),
    );
    let outpoint = |transaction: &Arc<Transaction>, index| OutPoint {
        hash: transaction.hash(),
        index,
    };
    let mut blocks = vec![genesis];
    let mut fan_out = Vec::new();
    for height in 1..=GENERATED_TIP {
        let mut transactions = Vec::new();
        if height == 101 {
            // A chain of spends within one block.
            let source = &blocks[1].transactions[0];
            let first = spending_transaction(outpoint(source, 0), source.outputs()[0].clone());
            let second = spending_transaction(outpoint(&first, 0), first.outputs()[0].clone());
            transactions.extend([first, second]);
        }
        if height == 102 {
            let source = &blocks[2].transactions[0];
            transactions.push(spending_transaction(
                outpoint(source, 2),
                source.outputs()[2].clone(),
            ));
            transactions.push(transparent_transaction(
                vec![outpoint(source, 0)],
                (0..FAN_OUT).map(indexed_output).collect(),
            ));
        }
        if height == 103 {
            let fan = blocks[102].transactions.last().unwrap().clone();
            fan_out = (0..FAN_OUT)
                .map(|index| {
                    spending_transaction(outpoint(&fan, index.into()), indexed_output(index))
                })
                .collect();
            transactions.extend(fan_out.iter().cloned());
        }
        if height == GENERATED_TIP {
            // One transaction spends outputs from many creating transactions.
            let mut sweep = indexed_output(0);
            sweep.value = Amount::try_from(u64::from(FAN_OUT)).unwrap();
            transactions.push(transparent_transaction(
                fan_out.iter().map(|creator| outpoint(creator, 0)).collect(),
                vec![sweep],
            ));
        }
        blocks.push(synthetic_block(
            blocks.last().unwrap(),
            height,
            transactions,
        ));
    }
    let network = generated_network(&blocks[0], None);
    (blocks, network)
}

/// A configured network for the generated chain, with NU7 at `nu7` if set.
fn generated_network(genesis: &Block, nu7: Option<u32>) -> Network {
    use zebra_chain::parameters::testnet::{
        ConfiguredActivationHeights, ConfiguredCheckpoints, ParametersBuilder,
    };

    let mut builder = ParametersBuilder::default()
        .with_genesis_hash(genesis.hash())
        .unwrap()
        .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(vec![
            (Height(0), genesis.hash()),
            (Height(3_000_000), zebra_chain::block::Hash([9; 32])),
        ]))
        .unwrap()
        .with_unshielded_coinbase_spends(true);
    if let Some(nu7) = nu7 {
        builder = builder
            .with_slow_start_interval(Height::MIN)
            .with_activation_heights(ConfiguredActivationHeights {
                nu7: Some(nu7),
                ..Default::default()
            })
            .unwrap()
            .with_initial_nsm_value_balance(Amount::try_from(100_000_000).unwrap())
            .clear_funding_streams()
            .with_lockbox_disbursements(vec![]);
    }
    builder.to_network().unwrap()
}

/// Omitted outputs get journal rows instead of UTXO rows until `H`.
#[test]
fn omitted_outputs_are_journaled_instead_of_written_as_utxos() {
    let _init_guard = zebra_test::init();
    let fixture = Fixture::generated(None);

    let (mut state, mut writer) = fixture.open();
    fixture.commit(&mut state, &mut writer, 0..=102);
    let fan = fixture.blocks[102].transactions.last().unwrap();
    let journal = entries(&state, SPENTNESS_JOURNAL);
    for index in 0..FAN_OUT {
        let outpoint = OutPoint {
            hash: fan.hash(),
            index: index.into(),
        };
        assert!(state.db.utxo(&outpoint).is_none());
        let location = OutputLocation::from_usize(Height(102), 2, index.into());
        assert!(journal
            .iter()
            .any(|(key, _)| *key == location.as_bytes().to_vec()));
    }
    // Survivors are written as usual.
    let coinbase = OutPoint {
        hash: fixture.blocks[102].transactions[0].hash(),
        index: 1,
    };
    assert!(state.db.utxo(&coinbase).is_some());
}

/// The block write task owns the writer that `init_with_spentness` feeds.
#[tokio::test(flavor = "multi_thread")]
async fn state_service_applies_hints() {
    let _init_guard = zebra_test::init();

    let fixture = Fixture::mainnet(None);
    let (results, read_state) = commit_through_state_service(&fixture).await;

    assert!(results
        .iter()
        .all(|result| matches!(result, Ok(crate::Response::Committed(_)))));
    let progress = read_progress(read_state.db()).unwrap().unwrap().1;
    assert_eq!(progress.phase, Phase::Complete);
}

/// An artifact defect fails the block, and then stops Zebra: the block write task panics,
/// rather than stopping quietly and leaving the sync stalled. Zebra's profiles abort on panic,
/// so the panic ends the process; tests unwind, and the last state handle re-raises it.
#[tokio::test(flavor = "multi_thread")]
#[should_panic(expected = "applying spentness hints failed")]
async fn state_service_stops_zebra_for_a_defective_block() {
    let _init_guard = zebra_test::init();

    let fixture = Fixture::mainnet(Some(OutputLocation::from_usize(Height(1), 0, 0)));
    let (results, read_state) = commit_through_state_service(&fixture).await;

    // The terminal block fails the count check, with the writer's error.
    assert!(results[..10].iter().all(Result::is_ok));
    let failure = results[10].as_ref().expect_err("the terminal block fails");
    assert!(
        format!("{failure:?}").contains("Spentness"),
        "the block fails with the spentness error: {failure:?}"
    );
    assert_eq!(read_state.db().finalized_tip_height(), Some(Height(9)));

    // Dropping the last state handle joins the block write task, and re-raises its panic.
    drop(read_state);
}

/// Commits `fixture`'s blocks through a state service that `init_with_spentness` feeds with its
/// artifact, and returns each commit's result and the read state.
async fn commit_through_state_service(
    fixture: &Fixture,
) -> (
    Vec<Result<crate::Response, crate::BoxError>>,
    crate::ReadStateService,
) {
    use tower::{Service, ServiceExt};

    use crate::Request;

    let (_sender, receiver) = watch::channel(Some(fixture.artifact.clone()));
    let (mut state, read_state, _, _) = crate::init_with_spentness(
        Config::ephemeral(),
        &fixture.network,
        Height::MAX,
        0,
        receiver,
    )
    .await;

    let mut results = Vec::new();
    for block in &fixture.blocks {
        let response = state
            .ready()
            .await
            .unwrap()
            .call(Request::CommitCheckpointVerifiedBlock(block.clone().into()));
        results.push(response.await);
    }

    (results, read_state)
}

/// `init_with_spentness` passes its max checkpoint height to the writer, which then
/// ignores an artifact whose `H` is above it.
#[tokio::test(flavor = "multi_thread")]
async fn state_service_ignores_an_artifact_above_the_max_checkpoint() {
    use tower::{Service, ServiceExt};

    use crate::{Request, Response};

    let _init_guard = zebra_test::init();
    let fixture = Fixture::mainnet(None);
    let (_sender, receiver) = watch::channel(Some(fixture.artifact.clone()));
    let (mut state, read_state, _, _) = crate::init_with_spentness(
        Config::ephemeral(),
        &fixture.network,
        (fixture.terminal_height() - 1).unwrap(),
        0,
        receiver,
    )
    .await;

    for block in &fixture.blocks {
        let response = state
            .ready()
            .await
            .unwrap()
            .call(Request::CommitCheckpointVerifiedBlock(block.clone().into()))
            .await;
        assert!(matches!(response, Ok(Response::Committed(_))));
    }
    assert_eq!(read_progress(read_state.db()).unwrap(), None);
    assert!(!read_state.has_incomplete_spentness_hints());
}

/// The spentness hint format is a reusable major upgrade, so no 29.x binary can
/// open a database that might hold a partial hinted state through its
/// minor-version downgrade path (F-342).
#[test]
fn spentness_format_is_a_reusable_major_upgrade() {
    use semver::Version;

    use crate::{
        config::database_format_version_on_disk,
        constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
        service::finalized_state::{restorable_db_versions, ZebraDb},
    };

    let _init_guard = zebra_test::init();
    let running_major = state_database_format_version_in_code().major;

    assert!(
        running_major > 29,
        "29.x readers must not share a directory with hinted state"
    );
    assert!(
        restorable_db_versions().contains(&running_major),
        "v29 state is reused without a resync"
    );

    // A 29.1.0 database (the format before spentness hints) moves out of `state/v29`.
    let fixture = Fixture::generated(None);
    let network = &fixture.network;
    let old_version = Version::new(29, 1, 0);
    let old_db = ZebraDb::new(
        &fixture.config,
        STATE_DATABASE_KIND,
        &old_version,
        network,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    old_db.update_format_version_on_disk(&old_version).unwrap();
    old_db.mark_finished_format_upgrades();
    drop(old_db);
    assert_eq!(
        database_format_version_on_disk(&fixture.config, STATE_DATABASE_KIND, 29, network).unwrap(),
        Some(old_version),
    );

    let (mut state, mut writer) = fixture.open();
    assert_eq!(
        database_format_version_on_disk(&fixture.config, STATE_DATABASE_KIND, 29, network).unwrap(),
        None,
        "no 29.x binary can open the moved database"
    );
    assert!(database_format_version_on_disk(
        &fixture.config,
        STATE_DATABASE_KIND,
        running_major,
        network
    )
    .unwrap()
    .is_some());

    // Hinted construction on the (empty) moved database only ever exists in the new directory.
    fixture.commit(&mut state, &mut writer, 0..=10);
    assert!(has_incomplete_hints(&state));
    assert!(!fixture
        .config
        .db_path(STATE_DATABASE_KIND, 29, network)
        .exists());
    assert!(fixture
        .config
        .db_path(STATE_DATABASE_KIND, running_major, network)
        .exists());
}
