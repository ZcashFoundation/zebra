//! After it prepares a block's commit, the block write task looks for its next message for a
//! short time before it sleeps.

use std::cell::Cell;

use super::{
    prepared::{root_state, run_writer},
    *,
};

/// Returns the output of `run`, and the number of messages it received without sleeping.
fn awake_receives<T>(run: impl FnOnce() -> T) -> (T, usize) {
    let before = AWAKE_RECEIVE_COUNT.with(Cell::get);
    let output = run();

    (output, AWAKE_RECEIVE_COUNT.with(Cell::get) - before)
}

/// Returns a message that the block write task can receive.
fn message() -> NonFinalizedWriteMessage {
    NonFinalizedWriteMessage::Invalidate {
        hash: block::Hash([0; 32]),
        rsp_tx: oneshot::channel().0,
    }
}

/// Sends a message from another thread after `pause`, and returns that thread.
fn send_after(
    sender: &tokio::sync::mpsc::UnboundedSender<NonFinalizedWriteMessage>,
    pause: Duration,
) -> std::thread::JoinHandle<()> {
    let sender = sender.clone();
    std::thread::spawn(move || {
        std::thread::sleep(pause);
        sender.send(message()).expect("the receiver is waiting");
    })
}

#[test]
fn next_message_is_received_without_sleeping_until_the_deadline() {
    let _init_guard = zebra_test::init();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let long_after = || Instant::now() + Duration::from_secs(60);

    // A message that is already queued is received at once, whatever the deadline.
    sender.send(message()).unwrap();
    let (received, awake) = awake_receives(|| recv_after_prepare(&mut receiver, Instant::now()));
    assert!(received.is_some());
    assert_eq!(awake, 1);

    // A message that arrives before the deadline is received without sleeping.
    let late = send_after(&sender, Duration::from_millis(20));
    let (received, awake) = awake_receives(|| recv_after_prepare(&mut receiver, long_after()));
    assert!(received.is_some());
    assert_eq!(awake, 1);
    late.join().unwrap();

    // A message that arrives after the deadline is received by sleeping until it arrives. The
    // receiver has to start waiting before the message is sent, so the pause is long enough for
    // a test thread that isn't scheduled for a while.
    let late = send_after(&sender, Duration::from_millis(500));
    let (received, awake) = awake_receives(|| recv_after_prepare(&mut receiver, Instant::now()));
    assert!(received.is_some());
    assert_eq!(awake, 0);
    late.join().unwrap();

    // A closed channel ends the wait, before and after the deadline.
    drop(sender);
    assert!(recv_after_prepare(&mut receiver, long_after()).is_none());
    assert!(recv_after_prepare(&mut receiver, Instant::now()).is_none());
}

#[test]
fn only_a_prepared_block_keeps_the_block_write_task_awake() {
    let _init_guard = zebra_test::init();

    // The commit request that follows a prepared block is received without sleeping, on a
    // machine with enough CPUs.
    let (finalized, state, root) = root_state();
    let child = root.make_fake_child();
    let (rsp_tx, mut rsp_rx) = oneshot::channel();
    let messages = vec![
        NonFinalizedWriteMessage::Prepare(child.clone(), None),
        NonFinalizedWriteMessage::Commit(
            (child.clone().prepare(), rsp_tx, None, None),
            Instant::now(),
        ),
    ];
    let (_state, awake) = awake_receives(|| run_writer(finalized, state, messages));
    if stays_awake_after_prepare() {
        assert_eq!(awake, 1);
    } else {
        // On a machine with fewer CPUs the task sleeps, so only the opposite can be checked.
        tracing::warn!(
            MIN_CPUS_TO_STAY_AWAKE,
            "too few CPUs to check that a prepared block keeps the block write task awake"
        );
        assert_eq!(awake, 0);
    }
    assert_eq!(rsp_rx.try_recv().unwrap().unwrap(), child.hash());

    // A block that can't be prepared leaves nothing to wait for, and neither does a commit.
    let (finalized, state, root) = root_state();
    let child = root.make_fake_child();
    let orphan = child.make_fake_child();
    let (rsp_tx, mut rsp_rx) = oneshot::channel();
    let messages = vec![
        NonFinalizedWriteMessage::Prepare(orphan, None),
        NonFinalizedWriteMessage::Commit(
            (child.clone().prepare(), rsp_tx, None, None),
            Instant::now(),
        ),
    ];
    let (_state, awake) = awake_receives(|| run_writer(finalized, state, messages));
    assert_eq!(awake, 0);
    assert_eq!(rsp_rx.try_recv().unwrap().unwrap(), child.hash());
}
