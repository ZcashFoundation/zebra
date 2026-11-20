//! The block write task must keep the recent headers of the block it committed last, and read a
//! block's ancestors from the state only when the block has another parent.

use std::cell::Cell;

use crate::service::check::RECENT_HEADERS_READS;

use super::{prepared::root_state, *};

/// Runs a block write task over `messages`, and returns the number of times it read recent
/// headers from the state.
fn recent_headers_reads(
    finalized: FinalizedState,
    state: NonFinalizedState,
    messages: Vec<NonFinalizedWriteMessage>,
) -> usize {
    let before = RECENT_HEADERS_READS.with(Cell::get);
    super::prepared::run_writer(finalized, state, messages);

    RECENT_HEADERS_READS.with(Cell::get) - before
}

/// Returns a commit message for `block`, and the receiver of its result.
fn commit(
    block: &Arc<Block>,
) -> (
    NonFinalizedWriteMessage,
    oneshot::Receiver<Result<block::Hash, crate::CommitSemanticallyVerifiedError>>,
) {
    let (rsp_tx, rsp_rx) = oneshot::channel();

    (
        NonFinalizedWriteMessage::Commit(
            (block.clone().prepare(), rsp_tx, None, None),
            Instant::now(),
        ),
        rsp_rx,
    )
}

#[test]
fn only_a_block_on_another_parent_reads_its_recent_headers() {
    let _init_guard = zebra_test::init();

    // The first block's headers are read, and each block after it is a child of the last one.
    let (finalized, state, root) = root_state();
    let child = root.make_fake_child();
    let grandchild = child.make_fake_child();
    let great_grandchild = grandchild.make_fake_child();
    let (messages, mut results): (Vec<_>, Vec<_>) = [&child, &grandchild, &great_grandchild]
        .into_iter()
        .map(commit)
        .unzip();
    assert_eq!(recent_headers_reads(finalized, state, messages), 1);
    for (result, block) in results
        .iter_mut()
        .zip([&child, &grandchild, &great_grandchild])
    {
        assert_eq!(result.try_recv().unwrap().unwrap(), block.hash());
    }

    // A prepared block's headers are kept when it is committed.
    let (finalized, state, root) = root_state();
    let child = root.make_fake_child();
    let grandchild = child.make_fake_child();
    let (child_commit, mut child_result) = commit(&child);
    let (grandchild_commit, mut grandchild_result) = commit(&grandchild);
    let messages = vec![
        NonFinalizedWriteMessage::Prepare(child.clone(), None),
        child_commit,
        grandchild_commit,
    ];
    assert_eq!(recent_headers_reads(finalized, state, messages), 1);
    assert_eq!(child_result.try_recv().unwrap().unwrap(), child.hash());
    assert_eq!(
        grandchild_result.try_recv().unwrap().unwrap(),
        grandchild.hash()
    );

    // A block committed with its proposal's verdict is the last committed block too.
    let (finalized, state, root) = root_state();
    let child = root.make_fake_child();
    let grandchild = child.make_fake_child();
    let great_grandchild = grandchild.make_fake_child();
    let mut validated = state.clone();
    for block in [&child, &grandchild] {
        validated
            .commit_block(block.clone().prepare(), &finalized.db)
            .unwrap();
    }
    let verdict = validated.best_chain().unwrap().tip_block().unwrap().clone();
    let (child_commit, mut child_result) = commit(&child);
    let (verdict_tx, mut verdict_result) = oneshot::channel();
    let (last_commit, mut last_result) = commit(&great_grandchild);
    let messages = vec![
        child_commit,
        NonFinalizedWriteMessage::Commit(
            (
                grandchild.clone().prepare(),
                verdict_tx,
                Some(Box::new(verdict)),
                None,
            ),
            Instant::now(),
        ),
        last_commit,
    ];
    assert_eq!(recent_headers_reads(finalized, state, messages), 1);
    assert_eq!(child_result.try_recv().unwrap().unwrap(), child.hash());
    assert_eq!(
        verdict_result.try_recv().unwrap().unwrap(),
        grandchild.hash()
    );
    assert_eq!(
        last_result.try_recv().unwrap().unwrap(),
        great_grandchild.hash()
    );

    // A block that was prepared but not committed isn't the last committed block, and neither
    // is the parent of a sibling.
    let (finalized, state, root) = root_state();
    let child = root.make_fake_child().set_work(1);
    let sibling = root.make_fake_child().set_work(2);
    let (sibling_commit, mut sibling_result) = commit(&sibling);
    let (child_commit, mut child_result) = commit(&child);
    let messages = vec![
        NonFinalizedWriteMessage::Prepare(child.clone(), None),
        sibling_commit,
        child_commit,
    ];
    assert_eq!(recent_headers_reads(finalized, state, messages), 3);
    assert_eq!(sibling_result.try_recv().unwrap().unwrap(), sibling.hash());
    assert_eq!(child_result.try_recv().unwrap().unwrap(), child.hash());
}
