//! The shared priority queue connecting `FairBuffer` handles to their worker.

use std::{
    hash::Hash,
    sync::{Arc, Mutex, MutexGuard},
};

use crossbeam_skiplist::SkipMap;
use tokio::sync::Notify;

use crate::{
    counts::RecentRequestCosts,
    error::{Closed, ServiceError, Shed},
    message::Message,
    BoxError,
};

/// The ordering key for queued requests: lowest key is dispatched first,
/// highest key is shed first.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct QueueKey {
    /// The caller's recent request count when the request was enqueued, or 0
    /// for internal requests.
    ///
    /// Queued requests keep their enqueue-time priority, even if the counts
    /// rotate while they are queued.
    priority: u64,

    /// A unique sequence number, for FIFO ordering between requests with the
    /// same priority.
    seq: u64,
}

/// A take-once cell for a queued message.
///
/// [`SkipMap`] entries only expose `&V`, so the message is wrapped in a
/// mutex to let whoever pops or sheds the entry take ownership of it. Each
/// cell is taken exactly once, by the (lock-serialized) queue operation that
/// removes its entry, so the inner lock is never contended.
type SlotCell<R, Fut> = Mutex<Option<Message<R, Fut>>>;

/// The state shared between `FairBuffer` handles and their worker.
///
/// # Correctness
///
/// All queue and state mutations happen while holding the [`Self::state`]
/// mutex, and the lock is never held across an `.await`. The [`SkipMap`]
/// provides the priority ordering; the mutex serializes mutations, which:
///
/// - makes the capacity check and shed exact: `State::len` always equals the
///   number of queued entries,
/// - closes the insert/teardown race: [`Self::fail`] sets `State::failed` and
///   drains the queue in one critical section, and [`Self::push`] checks
///   `State::failed` and inserts in one critical section. So every pushed
///   message is either drained by teardown or rejected before it is queued,
///   and no response future is left dangling.
pub(crate) struct Shared<K, R, Fut> {
    /// The queued messages, ordered by `(priority, FIFO sequence number)`.
    queue: SkipMap<QueueKey, SlotCell<R, Fut>>,

    /// The mutable queue state: sequence numbers, queue length, handle
    /// count, and the worker failure slot.
    state: Mutex<State>,

    /// Each caller's recent request cost, under its own lock so response
    /// futures can record response times without touching the queue state.
    ///
    /// The two locks are never held at the same time, so they can't
    /// deadlock.
    costs: Arc<Mutex<RecentRequestCosts<K>>>,

    /// Notifies the worker when a message is pushed.
    notify: Notify,

    /// The maximum number of queued messages before pushing sheds the
    /// highest-key queued message.
    capacity: usize,
}

/// The lock-protected part of [`Shared`].
#[derive(Debug)]
struct State {
    /// The number of live `FairBuffer` handles sharing this queue.
    ///
    /// When it reaches zero the worker shuts down after draining the queue,
    /// matching `tower::buffer`'s teardown when all senders drop.
    handles: usize,

    /// The sequence number for the next queued message.
    next_seq: u64,

    /// The number of entries in the queue.
    len: usize,

    /// The error that shut the fair buffer down, if any.
    ///
    /// `Some` means no further messages can be pushed, and the queue has been
    /// or is being drained.
    failed: Option<ServiceError>,
}

/// The result of pushing a message into the queue.
pub(crate) enum Push {
    /// The message was queued, or immediately shed if it was itself the
    /// highest-key message in a full queue. Either way, its response channel
    /// will be resolved.
    Queued,

    /// The fair buffer has shut down and the message was rejected, returning
    /// the error that shut it down.
    Failed(BoxError),
}

impl<K, R, Fut> Shared<K, R, Fut> {
    /// Returns a new empty queue with the given `capacity`.
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "fair buffer capacity must not be zero");

        Self {
            queue: SkipMap::new(),
            costs: Arc::new(Mutex::new(RecentRequestCosts::new())),
            state: Mutex::new(State {
                // `FairBuffer::pair` creates the first handle.
                handles: 1,
                next_seq: 0,
                len: 0,
                failed: None,
            }),
            notify: Notify::new(),
            capacity,
        }
    }

    /// Locks the shared state.
    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("a caller or the worker panicked while holding the fair buffer state lock")
    }

    /// Waits until [`Self::push`] queues a message.
    ///
    /// `Notify`'s stored permit means a push between the worker's last pop
    /// and this call completes the wait immediately, so wakeups can't be
    /// lost.
    pub(crate) async fn pushed(&self) {
        self.notify.notified().await;
    }

    /// Rotates the recent request costs, expiring costs recorded before
    /// the last rotation.
    pub(crate) fn rotate_counts(&self) {
        self.lock_costs().rotate();
    }

    /// Returns a handle to the recent request costs, for response futures to
    /// record response times.
    pub(crate) fn costs(&self) -> Arc<Mutex<RecentRequestCosts<K>>> {
        self.costs.clone()
    }

    /// Locks the recent request costs.
    fn lock_costs(&self) -> MutexGuard<'_, RecentRequestCosts<K>> {
        self.costs
            .lock()
            .expect("a caller or the worker panicked while holding the fair buffer costs lock")
    }

    /// Returns `Ok` if the fair buffer can accept requests, or the error
    /// that shut it down.
    pub(crate) fn check_open(&self) -> Result<(), BoxError> {
        match &self.lock_state().failed {
            None => Ok(()),
            Some(failed) => Err(failed.clone().into()),
        }
    }

    /// Records a cloned `FairBuffer` handle.
    pub(crate) fn handle_cloned(&self) {
        self.lock_state().handles += 1;
    }

    /// Records a dropped `FairBuffer` handle, waking the worker when the
    /// last handle drops so it can shut down.
    pub(crate) fn handle_dropped(&self) {
        let handles = {
            let mut state = self.lock_state();
            state.handles = state.handles.saturating_sub(1);
            state.handles
        };

        if handles == 0 {
            // Wake the worker outside the critical section; it re-checks
            // `handles_dropped` once the queue is drained.
            self.notify.notify_one();
        }
    }

    /// Returns true once every `FairBuffer` handle has been dropped, so no
    /// new messages can ever be pushed.
    pub(crate) fn handles_dropped(&self) -> bool {
        self.lock_state().handles == 0
    }
}

// The skip map's mutating methods are only available for `Send + 'static`
// values, because removed values can be dropped from any thread.
impl<K, R, Fut> Shared<K, R, Fut>
where
    R: Send + 'static,
    Fut: Send + 'static,
{
    /// Pushes a message from `key` into the queue, shedding the highest-key
    /// queued message if the queue is over capacity.
    ///
    /// Records `key`'s request count even if the pushed message is
    /// immediately shed: shed requests still count towards their sender's
    /// recent request count.
    pub(crate) fn push(&self, key: Option<K>, message: Message<R, Fut>) -> Push
    where
        K: Eq + Hash,
    {
        // The priority is the caller's recent request cost, read before the
        // queue state lock: the two locks are taken sequentially, never
        // nested. A request racing a rotation here gets a priority from one
        // side of the rotation, which is harmless — priorities are a
        // heuristic ordering, fixed at push time.
        let priority = match key {
            // Internal requests always have priority 0 and are never shed.
            None => 0,
            Some(key) => self.lock_costs().record_request(key),
        };

        {
            let mut state = self.lock_state();

            if let Some(failed) = &state.failed {
                return Push::Failed(failed.clone().into());
            }

            let seq = state.next_seq;
            state.next_seq = state.next_seq.wrapping_add(1);

            self.queue
                .insert(QueueKey { priority, seq }, Mutex::new(Some(message)));
            state.len += 1;

            if state.len > self.capacity {
                self.shed_highest(&mut state);
            }
        }

        // Wake the worker outside the critical section, so it doesn't
        // immediately block on the state lock.
        self.notify.notify_one();

        Push::Queued
    }

    /// Sheds the queued message with the highest `(priority, seq)` key,
    /// unless it is an internal request.
    ///
    /// If the queue only contains internal requests, nothing is shed and the
    /// queue is left over capacity: internal requests are never shed, and
    /// internal callers are trusted to be finite and disciplined.
    fn shed_highest(&self, state: &mut MutexGuard<'_, State>) {
        let Some(entry) = self.queue.back() else {
            return;
        };

        if entry.key().priority == 0 {
            return;
        }

        // The entry can't have been removed concurrently: all removals happen
        // under the state lock, which we hold.
        entry.remove();
        state.len = state.len.saturating_sub(1);

        if let Some(message) = take_message(entry.value()) {
            // Ignore send errors: a closed channel means the caller gave up
            // on the request, so it doesn't need a shed error.
            let _ = message.tx.send(Err(Shed::new().into()));
        }
    }

    /// Pops the lowest-key queued message that hasn't been canceled.
    ///
    /// Returns `None` when the queue is empty.
    pub(crate) fn pop_lowest(&self) -> Option<Message<R, Fut>> {
        let mut state = self.lock_state();

        while let Some(entry) = self.queue.pop_front() {
            state.len = state.len.saturating_sub(1);

            let Some(message) = take_message(entry.value()) else {
                continue;
            };

            // If the response channel is closed, the caller dropped its
            // response future, and nobody cares about the response.
            if message.tx.is_closed() {
                tracing::trace!("dropping cancelled fair buffer request");
                continue;
            }

            return Some(message);
        }

        None
    }

    /// Shuts the fair buffer down with `error`, failing all queued messages
    /// with a shared [`ServiceError`], and returns that error.
    ///
    /// If the fair buffer has already shut down, returns the original error
    /// and leaves the queue alone.
    pub(crate) fn fail(&self, error: BoxError) -> ServiceError {
        let mut state = self.lock_state();

        if let Some(failed) = &state.failed {
            // The fair buffer has already shut down with another error.
            return failed.clone();
        }

        let error = ServiceError::new(error);
        state.failed = Some(error.clone());

        // Drain the queue inside the critical section: `push` can't queue new
        // messages once `failed` is set, so the queue stays empty afterwards.
        while let Some(entry) = self.queue.pop_front() {
            state.len = state.len.saturating_sub(1);

            if let Some(message) = take_message(entry.value()) {
                // Ignore send errors from callers that gave up.
                let _ = message.tx.send(Err(error.clone().into()));
            }
        }

        error
    }

    /// Shuts the fair buffer down because the worker is gone, dropping all
    /// queued messages.
    ///
    /// Dropping a message closes its response channel, so its caller's
    /// response future fails with [`Closed`], matching `tower::buffer`'s
    /// teardown behaviour.
    pub(crate) fn close(&self) {
        let mut state = self.lock_state();

        if state.failed.is_some() {
            // The queue was already drained when the fair buffer shut down.
            return;
        }

        state.failed = Some(ServiceError::new(Closed::new().into()));

        while let Some(entry) = self.queue.pop_front() {
            state.len = state.len.saturating_sub(1);

            // Drop the message: its closed response channel fails the
            // caller's response future with `Closed`.
            drop(take_message(entry.value()));
        }
    }
}

/// Takes the message out of a queue entry's take-once cell.
///
/// Returns `None` if the message was already taken. This never happens under
/// the lock-serialized queue protocol, but it is handled defensively instead
/// of panicking.
fn take_message<R, Fut>(cell: &SlotCell<R, Fut>) -> Option<Message<R, Fut>> {
    cell.lock()
        .expect("a queue operation panicked while holding a fair buffer slot lock")
        .take()
}
