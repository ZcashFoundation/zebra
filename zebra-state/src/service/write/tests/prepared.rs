//! The block write task prepares a mining candidate's commit after answering the candidate. Only
//! the commit of the same block and hash list objects adds the prepared chain, and only while the
//! chain set is the one the chain was validated on.

use std::time::Instant;

use crate::{
    constants::MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT, service::chain_tip::LatestChainTip,
    CommitBlockError,
};

use super::*;

/// Counts of the block write task's prepared commits, and of the validations that reused a mining
/// candidate's note commitment trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Counts {
    /// Prepares that ran.
    prepares_run: usize,

    /// Prepares that kept a validated chain.
    prepares_stored: usize,

    /// Commits that added a prepared chain.
    prepared_commits: usize,

    /// Prepared chains that were for a commit's block, but not for the chain set it found.
    stale_prepared_commits: usize,

    /// Validations that reused a mining candidate's note commitment trees.
    reused_trees: usize,
}

/// Returns the counts since the process started.
///
/// Every block write task in the process adds to the same counts, so tests compare counts from
/// before and after what they check, in a process of their own.
fn counts() -> Counts {
    Counts {
        prepares_run: PREPARES_RUN.load(Ordering::SeqCst),
        prepares_stored: PREPARES_STORED.load(Ordering::SeqCst),
        prepared_commits: PREPARED_COMMITS.load(Ordering::SeqCst),
        stale_prepared_commits: STALE_PREPARED_COMMITS.load(Ordering::SeqCst),
        reused_trees: REUSED_TREES.load(Ordering::SeqCst),
    }
}

/// Returns `true` if `error` says that the block's parent isn't in the state.
fn is_not_ready(error: &CommitSemanticallyVerifiedError) -> bool {
    matches!(
        error.inner(),
        CommitBlockError::ValidateContextError(error)
            if matches!(**error, ValidateContextError::NotReadyToBeCommitted)
    )
}

/// A block write task on its own thread, and the channels that tests drive it with.
struct Writer {
    /// Sends messages to the task.
    sender: BlockWriteSender,

    /// The task's thread.
    worker: Option<Arc<std::thread::JoinHandle<()>>>,

    /// The blocks that the task rejected or committed.
    rejections: UnboundedReceiver<NonFinalizedWriteOutcome>,

    /// The task stops at startup if this receiver is dropped.
    _resets: UnboundedReceiver<(Option<block::Height>, block::Hash)>,

    /// The non-finalized state that the task published last.
    published: watch::Receiver<NonFinalizedState>,

    /// The task's finalized state.
    db: ZebraDb,

    /// Keeps the chain tip channels open.
    _chain_tip: (LatestChainTip, ChainTipChange),

    /// The block verifier's guard for each mining candidate, which stays open.
    guard: watch::Sender<()>,
}

impl Writer {
    /// Spawns a block write task with `finalized` and `state`.
    fn spawn(finalized: FinalizedState, state: NonFinalizedState) -> Self {
        let (chain_tip_sender, latest, change) = ChainTipSender::new(None, &state.network);
        let (state_sender, published) = watch::channel(state.clone());
        let db = finalized.db.clone();
        let (sender, resets, rejections, worker) = BlockWriteSender::spawn(
            finalized,
            state,
            chain_tip_sender,
            state_sender,
            false,
            None,
        );

        Self {
            sender,
            worker,
            rejections,
            _resets: resets,
            published,
            db,
            _chain_tip: (latest, change),
            guard: watch::channel(()).0,
        }
    }

    /// Closes the task's channels, and waits for its thread to exit.
    async fn join(self) {
        join_write_worker(self.sender, self.worker).await;
    }

    /// Returns the sender for the task's messages.
    fn tx(&self) -> &UnboundedSender<NonFinalizedWriteMessage> {
        self.sender
            .non_finalized
            .as_ref()
            .expect("BlockWriteSender::spawn always creates the non-finalized block sender")
    }

    /// Sends `block`'s block and hash list as a mining candidate, with an open guard, and
    /// returns the answer.
    async fn candidate(
        &self,
        block: &SemanticallyVerifiedBlock,
    ) -> Option<GetBlockTemplateChainInfo> {
        send_mining_candidate(self.tx(), block, self.guard.subscribe()).await
    }

    /// Waits until the task has run `prepares` prepares since the process started.
    async fn wait_for_prepares(&self, prepares: usize) {
        wait_for_prepares(self.tx(), prepares).await
    }

    /// Sends `block` to be committed, and returns the response.
    async fn commit(
        &self,
        block: SemanticallyVerifiedBlock,
    ) -> Result<block::Hash, CommitSemanticallyVerifiedError> {
        send_commit(self.tx(), block, None).await
    }

    /// Invalidates the block with `hash`, and returns the response.
    async fn invalidate(&self, hash: block::Hash) -> Result<block::Hash, InvalidateError> {
        let (rsp_tx, rsp_rx) = oneshot::channel();
        self.tx()
            .send(NonFinalizedWriteMessage::Invalidate { hash, rsp_tx })
            .unwrap();

        tokio::time::timeout(Duration::from_secs(10), rsp_rx)
            .await
            .expect("the write task answers")
            .expect("the write task is running")
    }

    /// Reconsiders the invalidated block with `hash`, and returns the response.
    async fn reconsider(&self, hash: block::Hash) -> Result<Vec<block::Hash>, ReconsiderError> {
        let (rsp_tx, rsp_rx) = oneshot::channel();
        self.tx()
            .send(NonFinalizedWriteMessage::Reconsider { hash, rsp_tx })
            .unwrap();

        tokio::time::timeout(Duration::from_secs(10), rsp_rx)
            .await
            .expect("the write task answers")
            .expect("the write task is running")
    }

    /// Returns once the task has handled every earlier message, and published the state they
    /// left.
    ///
    /// The task answers each message before it handles the next one, so this sends a message that
    /// changes nothing and waits for its answer. That message discards any prepared commit.
    async fn barrier(&self) {
        let invalidated = self.invalidate(block::Hash([0xff; 32])).await;
        assert!(invalidated.is_err(), "no block has this hash");
    }

    /// Gives `block` a header that is contextually valid on the published best tip, as a block
    /// template has.
    fn on_best_tip(&self, block: &Block) -> Arc<Block> {
        let snapshot = self.published();
        let context =
            read::difficulty::get_block_template_chain_info(&snapshot, &self.db, &snapshot.network)
                .expect("the state has a context for the next block");

        let mut block = block.clone();
        let header = Arc::make_mut(&mut block.header);
        header.previous_block_hash = context.tip_hash;
        header.time = context.min_time.into();
        header.difficulty_threshold = context.expected_difficulty;

        Arc::new(block)
    }

    /// Returns the non-finalized state that the task published last.
    fn published(&self) -> NonFinalizedState {
        self.published.borrow().clone()
    }

    /// Returns the hashes of the blocks that the task rejected since the last call.
    fn rejected(&mut self) -> Vec<block::Hash> {
        let mut rejected = Vec::new();
        while let Ok(outcome) = self.rejections.try_recv() {
            if let NonFinalizedWriteOutcome::Rejected(hash) = outcome {
                rejected.push(hash);
            }
        }

        rejected
    }
}

/// Runs a block write task over `messages` on this thread, until it has handled them all, and
/// returns the state it published last.
fn run_writer(
    finalized: FinalizedState,
    state: NonFinalizedState,
    messages: Vec<NonFinalizedWriteMessage>,
) -> NonFinalizedState {
    let (chain_tip_sender, _latest, _change) = ChainTipSender::new(None, &state.network);
    let (non_finalized_state_sender, published) = watch::channel(state.clone());
    let (queued, non_finalized_block_write_receiver) = tokio::sync::mpsc::unbounded_channel();
    for message in messages {
        assert!(queued.send(message).is_ok(), "the receiver is in scope");
    }
    drop(queued);
    let (invalid_block_reset_sender, _resets) = tokio::sync::mpsc::unbounded_channel();

    // Bind the task first: temporaries in a single `.run()` statement, such as the unused
    // finalized block sender, would live until `run()` returns and keep its first loop waiting.
    let task = WriteBlockWorkerTask {
        finalized_block_write_receiver: tokio::sync::mpsc::unbounded_channel().1,
        non_finalized_block_write_receiver,
        finalized_state: finalized,
        non_finalized_state: state,
        invalid_block_reset_sender,
        non_finalized_rejected_sender: tokio::sync::mpsc::unbounded_channel().0,
        chain_tip_sender,
        non_finalized_state_sender,
        backup_dir_path: None,
    };
    task.run();

    let published = published.borrow().clone();
    published
}

/// A mining candidate's block is committed with the chain that the block write task prepared,
/// which is the chain that a commit without the prepare adds.
#[tokio::test]
async fn mining_candidate_prepares_its_blocks_commit() {
    let _init_guard = zebra_test::init();
    let received_time = Instant::now();

    let mut published = Vec::new();
    for prepare in [true, false] {
        let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
        let block = block1_on_genesis(&state, &finalized.db);
        let mut commit = block.clone().prepare();
        commit.received_time = Some(received_time);
        let writer = Writer::spawn(finalized, state);

        let before = counts();
        if prepare {
            assert!(
                writer.candidate(&commit).await.is_some(),
                "a block that extends the best tip gets work",
            );
            writer.wait_for_prepares(before.prepares_run + 1).await;
        }
        assert_eq!(
            writer.commit(commit).await.expect("the block is valid"),
            block.hash()
        );

        assert_eq!(
            counts().prepared_commits,
            before.prepared_commits + usize::from(prepare),
            "only the commit after the candidate adds a prepared chain",
        );
        let state = writer.published();
        let tip = state
            .best_chain()
            .and_then(|chain| chain.tip_block())
            .expect("the block is committed");
        assert!(
            Arc::ptr_eq(&tip.block, &block),
            "the tip is the committed block object"
        );
        assert_eq!(
            tip.received_time,
            Some(received_time),
            "a prepared block is committed with its commit's received time",
        );
        published.push(state);

        writer.join().await;
    }

    assert!(
        published[0].eq_internal_state(&published[1]),
        "a prepared commit gives the state that a commit without it gives",
    );
}

/// A prepared chain is never added without its block's commit, so it can't be a parent either.
#[tokio::test]
async fn prepared_block_is_not_committed_without_its_commit() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block = block1_on_genesis(&state, &finalized.db);
    let child = block.make_fake_child();
    let writer = Writer::spawn(finalized, state);
    let state_before = writer.published();

    let before = counts();
    assert!(writer.candidate(&block.clone().prepare()).await.is_some());
    writer.wait_for_prepares(before.prepares_run + 1).await;
    assert_eq!(
        counts().prepares_stored,
        before.prepares_stored + 1,
        "the block's commit is prepared",
    );

    let error = writer
        .commit(child.prepare())
        .await
        .expect_err("the child's parent isn't committed");
    assert!(is_not_ready(&error), "unexpected error: {error:?}");

    let published = writer.published();
    assert!(!published.any_chain_contains(&block.hash()));
    assert!(
        published.eq_internal_state(&state_before),
        "a prepare doesn't change the state",
    );
    assert_eq!(counts().prepared_commits, before.prepared_commits);

    writer.join().await;
}

/// Only a commit of the block and hash list objects that the candidate was sent with adds the
/// prepared chain. A block hash doesn't identify the block's authorizing data, and equal hashes
/// in another list might not be the ones the prepared chain indexes.
#[tokio::test]
async fn only_the_prepared_block_object_commits_the_prepared_chain() {
    let _init_guard = zebra_test::init();

    type Commit = fn(&SemanticallyVerifiedBlock) -> SemanticallyVerifiedBlock;
    let cases: [(&str, Commit); 2] = [
        ("an equal block in another allocation", |candidate| {
            SemanticallyVerifiedBlock {
                block: Arc::new(Block::clone(&candidate.block)),
                ..candidate.clone()
            }
        }),
        ("an equal hash list in another allocation", |candidate| {
            SemanticallyVerifiedBlock {
                transaction_hashes: candidate.transaction_hashes.iter().copied().collect(),
                ..candidate.clone()
            }
        }),
    ];

    for (case, commit) in cases {
        let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
        let candidate = block1_on_genesis(&state, &finalized.db).prepare();
        let commit = commit(&candidate);
        assert_eq!(commit, candidate, "{case}: the commit has equal contents");
        let writer = Writer::spawn(finalized, state);

        let before = counts();
        assert!(writer.candidate(&candidate).await.is_some());
        writer.wait_for_prepares(before.prepares_run + 1).await;
        assert_eq!(
            counts().prepares_stored,
            before.prepares_stored + 1,
            "{case}: the candidate's commit is prepared",
        );

        let committed_block = commit.block.clone();
        assert_eq!(
            writer.commit(commit).await.expect("the block is valid"),
            candidate.hash,
        );
        assert_eq!(
            counts().prepared_commits,
            before.prepared_commits,
            "{case}: the commit validates the block itself",
        );
        let published = writer.published();
        let tip = published
            .best_chain()
            .and_then(|chain| chain.tip_block())
            .expect("the block is committed");
        assert!(
            Arc::ptr_eq(&tip.block, &committed_block),
            "{case}: the tip is the committed block object",
        );

        writer.join().await;
    }
}

/// A message between a candidate and its block's commit discards the prepared commit, because
/// the chain set the block was validated on might have changed.
#[tokio::test]
async fn request_between_candidate_and_commit_discards_the_prepared_commit() {
    let _init_guard = zebra_test::init();

    // An invalidation removes the prepared block's parent.
    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block1 = block1_on_genesis(&state, &finalized.db);
    let writer = Writer::spawn(finalized, state);
    writer
        .commit(block1.clone().prepare())
        .await
        .expect("block 1 is valid");
    let child = writer.on_best_tip(&block1.make_fake_child());
    writer
        .commit(child.clone().prepare())
        .await
        .expect("the child is valid");
    let grandchild = writer.on_best_tip(&child.make_fake_child()).prepare();

    let before = counts();
    assert!(writer.candidate(&grandchild).await.is_some());
    writer.wait_for_prepares(before.prepares_run + 1).await;
    assert_eq!(counts().prepares_stored, before.prepares_stored + 1);

    assert_eq!(
        writer
            .invalidate(child.hash())
            .await
            .expect("the child is in a chain"),
        child.hash()
    );
    let grandchild_hash = grandchild.hash;
    let error = writer
        .commit(grandchild)
        .await
        .expect_err("the grandchild's parent is invalidated");
    assert!(is_not_ready(&error), "unexpected error: {error:?}");

    let published = writer.published();
    assert_eq!(
        published.best_tip(),
        Some((Height(1), block1.hash())),
        "the invalidated child's parent is the tip",
    );
    assert!(!published.any_chain_contains(&grandchild_hash));
    assert_eq!(
        (counts().prepared_commits, counts().stale_prepared_commits),
        (before.prepared_commits, before.stale_prepared_commits),
        "the invalidation discards the prepared commit",
    );

    writer.join().await;

    // A reconsideration adds a chain to the chain set.
    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block1 = block1_on_genesis(&state, &finalized.db);
    let writer = Writer::spawn(finalized, state);
    writer
        .commit(block1.clone().prepare())
        .await
        .expect("block 1 is valid");
    let child = writer.on_best_tip(&block1.make_fake_child());
    writer
        .commit(child.clone().prepare())
        .await
        .expect("the child is valid");
    assert_eq!(
        writer
            .invalidate(child.hash())
            .await
            .expect("the child is in a chain"),
        child.hash()
    );
    // The block write task publishes the invalidation after answering it.
    writer.barrier().await;

    let second_child = writer
        .on_best_tip(&block1.make_fake_child().set_block_commitment([2; 32]))
        .prepare();
    assert_ne!(second_child.hash, child.hash());

    let before = counts();
    assert!(writer.candidate(&second_child).await.is_some());
    writer.wait_for_prepares(before.prepares_run + 1).await;
    assert_eq!(counts().prepares_stored, before.prepares_stored + 1);

    assert!(writer
        .reconsider(child.hash())
        .await
        .expect("the child was invalidated")
        .contains(&child.hash()));
    let second_child_hash = second_child.hash;
    assert_eq!(
        writer
            .commit(second_child)
            .await
            .expect("the second child is valid"),
        second_child_hash,
    );

    let published = writer.published();
    assert!(published.any_chain_contains(&child.hash()));
    assert!(published.any_chain_contains(&second_child_hash));
    assert_eq!(
        (counts().prepared_commits, counts().stale_prepared_commits),
        (before.prepared_commits, before.stale_prepared_commits),
        "the reconsideration discards the prepared commit",
    );

    writer.join().await;
}

/// A later mining candidate replaces the prepared commit of an earlier one.
#[tokio::test]
async fn another_candidate_replaces_the_prepared_commit() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block = block1_on_genesis(&state, &finalized.db);
    let sibling = nonce_variant(&block, [0xff; 32]);
    let (block, sibling) = (block.prepare(), sibling.prepare());
    let writer = Writer::spawn(finalized, state);

    let before = counts();
    assert!(writer.candidate(&block).await.is_some());
    writer.wait_for_prepares(before.prepares_run + 1).await;
    assert!(writer.candidate(&sibling).await.is_some());
    writer.wait_for_prepares(before.prepares_run + 2).await;
    assert_eq!(counts().prepares_stored, before.prepares_stored + 2);

    let sibling_hash = sibling.hash;
    assert_eq!(
        writer.commit(sibling).await.expect("the sibling is valid"),
        sibling_hash
    );
    assert_eq!(
        counts().prepared_commits,
        before.prepared_commits + 1,
        "the commit of the latest candidate's block adds its prepared chain",
    );

    let block_hash = block.hash;
    assert_eq!(
        writer.commit(block).await.expect("the block is valid"),
        block_hash
    );
    assert_eq!(
        counts().prepared_commits,
        before.prepared_commits + 1,
        "the earlier candidate's prepared commit was replaced",
    );

    let published = writer.published();
    assert!(published.any_chain_contains(&sibling_hash));
    assert!(
        published.any_chain_contains(&block_hash),
        "the earlier block is committed as a fork",
    );

    writer.join().await;
}

/// The block write task doesn't prepare the commit of a block that can't be validated before its
/// transactions are verified, and a block it can't prepare doesn't stop it.
#[tokio::test]
async fn unverifiable_block_is_not_prepared() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block1 = block1_on_genesis(&state, &finalized.db);
    let mut writer = Writer::spawn(finalized, state);
    writer
        .commit(block1.clone().prepare())
        .await
        .expect("block 1 is valid");

    // These blocks all have the child's valid header. Their bodies don't match the header's
    // Merkle root, which the state doesn't check, so they are never committed.
    let child = writer.on_best_tip(&block1.make_fake_child());

    let mut repeated_coinbase = Block::clone(&child);
    repeated_coinbase
        .transactions
        .push(child.transactions[0].clone());

    let mut repeated_parent_transaction = Block::clone(&child);
    repeated_parent_transaction
        .transactions
        .push(block1.transactions[0].clone());

    let mut short_hash_list = child.clone().prepare();
    let (_last, hashes) = short_hash_list
        .transaction_hashes
        .split_last()
        .expect("a block has a coinbase transaction");
    short_hash_list.transaction_hashes = hashes.into();

    for (case, candidate) in [
        (
            "a block that repeats its coinbase transaction",
            Arc::new(repeated_coinbase).prepare(),
        ),
        (
            "a block that repeats its parent's coinbase transaction",
            Arc::new(repeated_parent_transaction).prepare(),
        ),
        (
            "a block with a hash list that misses its last transaction",
            short_hash_list,
        ),
    ] {
        assert_eq!(candidate.hash, child.hash());

        let before = counts();
        assert!(
            writer.candidate(&candidate).await.is_some(),
            "{case}: the block's header gets work",
        );
        writer.wait_for_prepares(before.prepares_run + 1).await;
        writer.barrier().await;
        assert_eq!(
            counts(),
            Counts {
                prepares_run: before.prepares_run + 1,
                ..before
            },
            "{case}: the block's commit isn't prepared",
        );

        assert!(
            !writer.rejected().contains(&child.hash()),
            "{case}: a block that can't be prepared isn't rejected",
        );
        let error = writer
            .commit(child.make_fake_child().prepare())
            .await
            .expect_err("the block isn't committed");
        assert!(
            is_not_ready(&error),
            "{case}: a block that can't be prepared doesn't reject its children: {error:?}",
        );
    }

    let mut without_transactions = Block::clone(&block1);
    without_transactions.transactions.clear();
    assert!(
        prepare_commit(
            &writer.db,
            &writer.published(),
            Arc::new(without_transactions),
            Arc::from([]),
        )
        .is_none(),
        "a block without a coinbase height isn't prepared",
    );

    writer.join().await;
}

/// A commit of another block discards the prepared commit, because it changes the chain set the
/// block was validated on.
#[tokio::test]
async fn commit_of_another_block_discards_the_prepared_commit() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block = block1_on_genesis(&state, &finalized.db);
    let sibling = nonce_variant(&block, [0xff; 32]);
    let block = block.prepare();
    let writer = Writer::spawn(finalized, state);

    let before = counts();
    assert!(writer.candidate(&block).await.is_some());
    writer.wait_for_prepares(before.prepares_run + 1).await;
    assert_eq!(counts().prepares_stored, before.prepares_stored + 1);

    assert_eq!(
        writer
            .commit(sibling.clone().prepare())
            .await
            .expect("the sibling is valid"),
        sibling.hash(),
    );
    let block_hash = block.hash;
    assert_eq!(
        writer.commit(block).await.expect("the block is valid"),
        block_hash
    );

    let published = writer.published();
    assert!(published.any_chain_contains(&sibling.hash()));
    assert!(published.any_chain_contains(&block_hash));
    assert_eq!(
        (counts().prepared_commits, counts().stale_prepared_commits),
        (before.prepared_commits, before.stale_prepared_commits),
        "the block was prepared on a state without its sibling",
    );

    writer.join().await;
}

/// A block prepared on the finalized tip is committed as a new chain.
#[tokio::test]
async fn prepared_block_on_the_finalized_tip_starts_a_chain() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    assert!(state.is_chain_set_empty());
    let block = block1_on_genesis(&state, &finalized.db).prepare();
    let writer = Writer::spawn(finalized, state);

    let before = counts();
    assert!(writer.candidate(&block).await.is_some());
    writer.wait_for_prepares(before.prepares_run + 1).await;
    let block_hash = block.hash;
    assert_eq!(
        writer.commit(block).await.expect("the block is valid"),
        block_hash
    );

    assert_eq!(counts().prepared_commits, before.prepared_commits + 1);
    let published = writer.published();
    assert_eq!(published.chain_count(), 1);
    assert_eq!(published.best_tip(), Some((Height(1), block_hash)));

    writer.join().await;
}

/// A commit that finalizes blocks discards the prepared commit of a block that was validated
/// before them.
#[tokio::test]
async fn prepared_commit_is_discarded_when_a_commit_finalizes_blocks() {
    let _init_guard = zebra_test::init();

    let (finalized, mut state, genesis) = new_state_with_mainnet_genesis();
    finalized.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    let mut best = vec![genesis.block.make_fake_child().set_work(10)];
    state
        .commit_new_chain(best[0].clone().prepare(), &finalized.db)
        .unwrap();
    let min_blocks = usize::try_from(MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT)
        .expect("the minimum fits in a usize");
    while best.len() < min_blocks + 2 {
        let tip = best.last().unwrap().make_fake_child().set_work(10);
        state
            .commit_block(tip.clone().prepare(), &finalized.db)
            .unwrap();
        best.push(tip);
    }

    // The block write task finalizes the best chain down to the minimum before it handles any
    // message, and then once each commit makes the chain longer.
    state.set_max_index_entries(Some(1));
    let writer = Writer::spawn(finalized, state);
    writer.barrier().await;
    let finalized_before = writer.db.finalized_tip_height();

    // The chain is longer than the difficulty adjustment span, so the blocks need valid headers.
    let tip = best.last().expect("the chain has blocks");
    let block = writer
        .on_best_tip(&tip.make_fake_child().set_block_commitment([1; 32]))
        .prepare();
    let sibling = writer
        .on_best_tip(&tip.make_fake_child().set_block_commitment([2; 32]))
        .prepare();

    let before = counts();
    assert!(writer.candidate(&block).await.is_some());
    writer.wait_for_prepares(before.prepares_run + 1).await;
    assert_eq!(counts().prepares_stored, before.prepares_stored + 1);

    let sibling_hash = sibling.hash;
    assert_eq!(
        writer.commit(sibling).await.expect("the sibling is valid"),
        sibling_hash
    );
    let block_hash = block.hash;
    assert_eq!(
        writer.commit(block).await.expect("the block is valid"),
        block_hash
    );

    // The block write task finalizes blocks after it answers a commit. A message between the
    // two commits would discard the prepared commit, so this waits until after them.
    writer.barrier().await;
    assert!(
        writer.db.finalized_tip_height() > finalized_before,
        "the sibling's commit finalizes a block",
    );

    let published = writer.published();
    assert!(published.any_chain_contains(&sibling_hash));
    assert!(
        published.any_chain_contains(&block_hash),
        "the block is committed as a fork",
    );
    assert_eq!(
        (counts().prepared_commits, counts().stale_prepared_commits),
        (before.prepared_commits, before.stale_prepared_commits),
        "the sibling's commit discards the prepared commit",
    );
    let finalized_tip = writer
        .db
        .finalized_tip_height()
        .expect("the finalized state has blocks");
    assert!(
        published
            .chain_iter()
            .all(|chain| chain.non_finalized_root_height() > finalized_tip),
        "no chain has a finalized block",
    );

    writer.join().await;
}

/// A message that is already waiting when the block write task answers a mining candidate skips
/// the candidate's prepare, so it doesn't wait for the prepare. The block's commit still reuses
/// the candidate's work.
#[test]
fn queued_message_skips_the_prepare() {
    let _init_guard = zebra_test::init();

    // With the candidate.
    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block = block1_on_genesis(&state, &finalized.db).prepare();
    let (_guard, verification) = watch::channel(());
    let (candidate_tx, mut candidate_rx) = oneshot::channel();
    let (commit_tx, mut commit_rx) = oneshot::channel();
    let messages = vec![
        NonFinalizedWriteMessage::MiningCandidate {
            block: block.block.clone(),
            transaction_hashes: block.transaction_hashes.clone(),
            verification,
            rsp_tx: candidate_tx,
        },
        NonFinalizedWriteMessage::Commit((block.clone(), commit_tx, None)),
    ];

    let before = counts();
    let with_candidate = run_writer(finalized, state, messages);

    assert!(
        candidate_rx
            .try_recv()
            .expect("the block write task answers")
            .is_some(),
        "a block that extends the best tip gets work",
    );
    assert_eq!(
        commit_rx
            .try_recv()
            .expect("the block write task answers")
            .expect("the block is valid"),
        block.hash,
    );
    assert_eq!(
        counts(),
        Counts {
            reused_trees: before.reused_trees + 1,
            ..before
        },
        "the commit reuses the candidate's work without a prepare",
    );

    // Without the candidate.
    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let (commit_tx, mut commit_rx) = oneshot::channel();
    let without_candidate = run_writer(
        finalized,
        state,
        vec![NonFinalizedWriteMessage::Commit((
            block.clone(),
            commit_tx,
            None,
        ))],
    );
    assert_eq!(
        commit_rx
            .try_recv()
            .expect("the block write task answers")
            .expect("the block is valid"),
        block.hash,
    );

    assert!(with_candidate.eq_internal_state(&without_candidate));
}

/// The block write task doesn't prepare the commit of a block whose verifier is done with it.
#[test]
fn closed_verification_skips_the_prepare() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block = block1_on_genesis(&state, &finalized.db).prepare();
    let (guard, verification) = watch::channel(());
    drop(guard);
    let (rsp_tx, mut rsp_rx) = oneshot::channel();

    // No other message is waiting, so only the closed guard skips the prepare.
    let before = counts();
    run_writer(
        finalized,
        state,
        vec![NonFinalizedWriteMessage::MiningCandidate {
            block: block.block.clone(),
            transaction_hashes: block.transaction_hashes.clone(),
            verification,
            rsp_tx,
        }],
    );

    assert!(
        rsp_rx
            .try_recv()
            .expect("the block write task answers")
            .is_some(),
        "a block that extends the best tip gets work",
    );
    assert_eq!(counts(), before, "the block's commit isn't prepared");
}

/// The block write task doesn't prepare the commit of a block that got no early mining work.
#[test]
fn candidate_without_work_skips_the_prepare() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    // Mainnet block 2's parent isn't the best tip, so the block gets no early mining work.
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .expect("block is valid");
    let block = block.prepare();
    let (_guard, verification) = watch::channel(());
    let (rsp_tx, mut rsp_rx) = oneshot::channel();

    // No other message is waiting, and the guard is open, so only the missing work skips the
    // prepare.
    let before = counts();
    run_writer(
        finalized,
        state,
        vec![NonFinalizedWriteMessage::MiningCandidate {
            block: block.block.clone(),
            transaction_hashes: block.transaction_hashes.clone(),
            verification,
            rsp_tx,
        }],
    );

    assert_eq!(
        rsp_rx.try_recv().expect("the block write task answers"),
        None,
        "a block that doesn't extend the best tip gets no work",
    );
    assert_eq!(counts(), before, "the block's commit isn't prepared");
}
