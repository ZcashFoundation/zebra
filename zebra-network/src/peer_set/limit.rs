//! Counting active connections used by Zebra.
//!
//! These types can be used to count any kind of active resource.
//! But they are currently used to track the number of open connections.

use std::{
    fmt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use tokio::sync::mpsc;

/// A counter of the slots of a bounded resource that are currently held.
///
/// Slots are claimed with [`claim`](Self::claim) or
/// [`try_claim`](Self::try_claim), and released when the returned
/// [`SlotGuard`] is dropped, on every exit path.
#[derive(Clone, Debug, Default)]
pub struct SlotCounter(Arc<AtomicUsize>);

impl SlotCounter {
    /// Returns the number of slots currently held.
    pub fn count(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    /// Claims a slot unconditionally.
    pub fn claim(&self) -> SlotGuard {
        self.0.fetch_add(1, Ordering::AcqRel);
        SlotGuard(self.0.clone())
    }

    /// Claims a slot, unless `limit` slots are already held.
    pub fn try_claim(&self, limit: usize) -> Option<SlotGuard> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < limit).then_some(held + 1)
            })
            .ok()
            .map(|_| SlotGuard(self.0.clone()))
    }
}

/// A claimed slot of a [`SlotCounter`], released when it is dropped.
#[derive(Debug)]
pub struct SlotGuard(Arc<AtomicUsize>);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A signal sent by a [`Connection`][1] when it opens or closes.
///
/// Used to count the number of open connections.
///
/// [1]: crate::peer::Connection
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum ConnectionStatus {
    Opened,
    Closed,
}

/// A counter for active connections.
///
/// Creates a [`ConnectionTracker`] to track each active connection.
/// When these trackers are dropped, the counter gets notified.
pub struct ActiveConnectionCounter {
    /// The number of active connections tracked using this counter.
    count: usize,

    /// The number of connection slots that are reserved for connection attempts.
    reserved_count: usize,

    /// The limit for this type of connection, for diagnostics only.
    /// The caller must enforce the limit by ignoring, delaying, or dropping connections.
    limit: usize,

    /// The label for this connection counter, typically its type.
    label: Arc<str>,

    /// The channel used to send opened or closed connection notifications.
    status_notification_tx: mpsc::UnboundedSender<ConnectionStatus>,

    /// The channel used to receive opened or closed connection notifications.
    status_notification_rx: mpsc::UnboundedReceiver<ConnectionStatus>,

    /// Active connection count progress transmitter.
    #[cfg(feature = "progress-bar")]
    connection_bar: howudoin::Tx,
}

impl fmt::Debug for ActiveConnectionCounter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActiveConnectionCounter")
            .field("label", &self.label)
            .field("count", &self.count)
            .field("reserved_count", &self.reserved_count)
            .field("limit", &self.limit)
            .finish()
    }
}

impl ActiveConnectionCounter {
    /// Create and return a new active connection counter.
    pub fn new_counter() -> Self {
        Self::new_counter_with(usize::MAX, "Active Connections")
    }

    /// Create and return a new active connection counter with `limit` and `label`.
    /// The caller must check and enforce limits using [`update_count()`](Self::update_count).
    pub fn new_counter_with<S: ToString>(limit: usize, label: S) -> Self {
        // The number of items in this channel is bounded by the connection limit.
        let (status_notification_tx, status_notification_rx) = mpsc::unbounded_channel();

        let label = label.to_string();

        #[cfg(feature = "progress-bar")]
        let connection_bar = howudoin::new_root().label(label.clone());

        Self {
            count: 0,
            reserved_count: 0,
            limit,
            label: label.into(),
            status_notification_rx,
            status_notification_tx,
            #[cfg(feature = "progress-bar")]
            connection_bar,
        }
    }

    /// Create and return a new [`ConnectionTracker`], and add 1 to this counter.
    ///
    /// When the returned tracker is dropped, this counter will be notified, and decreased by 1.
    pub fn track_connection(&mut self) -> ConnectionTracker {
        ConnectionTracker::new(self)
    }

    /// Check for closed connection notifications, and return the current connection count.
    pub fn update_count(&mut self) -> usize {
        let previous_connections = self.count;

        // We ignore errors here:
        // - TryRecvError::Empty means that there are no pending close notifications
        // - TryRecvError::Closed is unreachable, because we hold a sender
        while let Ok(status) = self.status_notification_rx.try_recv() {
            match status {
                ConnectionStatus::Opened => {
                    self.reserved_count -= 1;
                    self.count += 1;

                    debug!(
                        open_connections = ?self.count,
                        ?previous_connections,
                        limit = ?self.limit,
                        label = ?self.label,
                        "a peer connection was opened",
                    );
                }
                ConnectionStatus::Closed => {
                    self.count -= 1;

                    debug!(
                        open_connections = ?self.count,
                        ?previous_connections,
                        limit = ?self.limit,
                        label = ?self.label,
                        "a peer connection was closed",
                    );
                }
            }
        }

        trace!(
            open_connections = ?self.count,
            ?previous_connections,
            limit = ?self.limit,
            label = ?self.label,
            "updated active connection count",
        );

        #[cfg(feature = "progress-bar")]
        self.connection_bar
            .set_pos(u64::try_from(self.count).expect("fits in u64"));
        // .set_len(u64::try_from(self.limit).expect("fits in u64"));

        self.count + self.reserved_count
    }
}

impl Drop for ActiveConnectionCounter {
    fn drop(&mut self) {
        #[cfg(feature = "progress-bar")]
        self.connection_bar.close();
    }
}

/// A connection counter shared by the tasks that open connections of the
/// same kind, counting with atomic slots instead of a single owner.
///
/// The legacy and version 2 transports accept and dial connections from
/// separate tasks, but their connections share one limit, so they must share
/// one counter: separate counters would let each transport fill the limit on
/// its own, doubling the connections an operator configured.
#[derive(Clone, Debug)]
pub struct SharedConnectionCounter {
    /// The reserved and open connection slots currently held.
    slots: SlotCounter,

    /// The number of slots whose trackers have been marked open, for
    /// diagnostics.
    open: Arc<AtomicUsize>,

    /// The limit for this type of connection, for diagnostics only.
    /// The caller must enforce the limit by ignoring, delaying, or dropping
    /// connections, or by reserving slots with
    /// [`try_track_connection`](Self::try_track_connection).
    limit: usize,

    /// The label for this connection counter, typically its type.
    label: Arc<str>,

    /// Active connection count progress bar, closed when the last clone of
    /// this counter is dropped.
    #[cfg(feature = "progress-bar")]
    connection_bar: Arc<ConnectionBar>,
}

/// Closes the progress bar of a [`SharedConnectionCounter`] when its last
/// clone is dropped.
#[cfg(feature = "progress-bar")]
struct ConnectionBar(howudoin::Tx);

// `howudoin::Tx` does not implement `Debug`, so it cannot be derived.
#[cfg(feature = "progress-bar")]
impl fmt::Debug for ConnectionBar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ConnectionBar").finish()
    }
}

#[cfg(feature = "progress-bar")]
impl Drop for ConnectionBar {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl SharedConnectionCounter {
    /// Create and return a new shared connection counter with `limit` and
    /// `label`. The caller must check and enforce limits using
    /// [`update_count()`](Self::update_count), or reserve slots atomically
    /// with [`try_track_connection`](Self::try_track_connection).
    pub fn new_counter_with<S: ToString>(limit: usize, label: S) -> Self {
        let label = label.to_string();

        #[cfg(feature = "progress-bar")]
        let connection_bar = Arc::new(ConnectionBar(howudoin::new_root().label(label.clone())));

        Self {
            slots: SlotCounter::default(),
            open: Arc::new(AtomicUsize::new(0)),
            limit,
            label: label.into(),
            #[cfg(feature = "progress-bar")]
            connection_bar,
        }
    }

    /// Returns the current connection count, including reserved slots for
    /// connection attempts.
    pub fn update_count(&self) -> usize {
        let count = self.slots.count();

        trace!(
            open_connections = ?count,
            limit = ?self.limit,
            label = ?self.label,
            "updated active connection count",
        );

        #[cfg(feature = "progress-bar")]
        self.connection_bar
            .0
            .set_pos(u64::try_from(self.open.load(Ordering::Acquire)).expect("fits in u64"));

        count
    }

    /// Create and return a new [`ConnectionTracker`], and add 1 to this counter.
    ///
    /// When the returned tracker is dropped, this counter is decreased by 1.
    pub fn track_connection(&self) -> ConnectionTracker {
        ConnectionTracker::new_shared(self, self.slots.claim())
    }

    /// Atomically checks the current connection count against `limit`, and
    /// creates and returns a new [`ConnectionTracker`] if the count is below
    /// the limit.
    ///
    /// # Security
    ///
    /// The check and the reservation are one atomic update, so tasks sharing
    /// this counter cannot both pass the check at the last free slot and
    /// overshoot the limit.
    pub fn try_track_connection(&self, limit: usize) -> Option<ConnectionTracker> {
        let slot = self.slots.try_claim(limit)?;
        Some(ConnectionTracker::new_shared(self, slot))
    }
}

/// A per-connection tracker.
///
/// [`ActiveConnectionCounter`] and [`SharedConnectionCounter`] create a
/// tracker instance for each active connection. When these trackers are
/// dropped, the counter is notified or decreased.
pub struct ConnectionTracker {
    /// The counter this tracker reports to.
    counter: TrackerCounter,

    /// A flag indicating whether this connection tracker has already counted the
    /// connection as opened, so it is not double-counted.
    has_marked_open: bool,

    /// The label for this connection counter, typically its type.
    label: Arc<str>,
}

/// The counter a [`ConnectionTracker`] reports to.
enum TrackerCounter {
    /// A single-owner [`ActiveConnectionCounter`], notified over its channel.
    Channel {
        /// The channel used to send open connection status notifications on first response or
        /// closed connection notifications on drop.
        status_notification_tx: mpsc::UnboundedSender<ConnectionStatus>,
    },

    /// A [`SharedConnectionCounter`], holding one of its atomic slots.
    Shared {
        /// The held slot; dropping it decreases the shared count.
        _slot: SlotGuard,

        /// The shared counter's open-connection diagnostic count.
        open: Arc<AtomicUsize>,
    },
}

impl fmt::Debug for ConnectionTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ConnectionTracker")
            .field(&self.label)
            .finish()
    }
}

impl ConnectionTracker {
    /// Counts the connection as open, exactly once: later calls and the
    /// implicit call on drop are ignored.
    pub fn mark_open(&mut self) {
        if !self.has_marked_open {
            self.has_marked_open = true;

            match &self.counter {
                TrackerCounter::Channel {
                    status_notification_tx,
                } => {
                    let _ = status_notification_tx.send(ConnectionStatus::Opened);
                }
                TrackerCounter::Shared { open, .. } => {
                    open.fetch_add(1, Ordering::AcqRel);
                    debug!(label = ?self.label, "a peer connection was opened");
                }
            }
        }
    }

    /// Create and return a new active connection tracker, and add 1 to `counter`.
    /// All connection trackers share a label with their connection counter.
    ///
    /// When the returned tracker is dropped, `counter` will be notified, and decreased by 1.
    fn new(counter: &mut ActiveConnectionCounter) -> Self {
        counter.reserved_count += 1;

        debug!(
            open_connections = ?counter.count,
            limit = ?counter.limit,
            label = ?counter.label,
            "opening a new peer connection",
        );

        Self {
            counter: TrackerCounter::Channel {
                status_notification_tx: counter.status_notification_tx.clone(),
            },
            has_marked_open: false,
            label: counter.label.clone(),
        }
    }

    /// Create and return a new active connection tracker holding `slot` of
    /// `counter`'s slots.
    ///
    /// When the returned tracker is dropped, the slot is released, and the
    /// shared count decreases by 1.
    fn new_shared(counter: &SharedConnectionCounter, slot: SlotGuard) -> Self {
        debug!(
            open_connections = ?counter.slots.count(),
            limit = ?counter.limit,
            label = ?counter.label,
            "opening a new peer connection",
        );

        Self {
            counter: TrackerCounter::Shared {
                _slot: slot,
                open: counter.open.clone(),
            },
            has_marked_open: false,
            label: counter.label.clone(),
        }
    }
}

impl Drop for ConnectionTracker {
    /// Notifies the corresponding connection counter that the connection has closed.
    fn drop(&mut self) {
        debug!(label = ?self.label, "closing a peer connection");

        // # Correctness
        //
        // Unopened connections are opened just before being closed, so both
        // the pending-attempt count and the open count are balanced.
        self.mark_open();

        match &self.counter {
            // We ignore disconnected errors, because the receiver can be dropped
            // before some connections are dropped.
            //
            // # Security
            //
            // This channel is actually bounded by the inbound and outbound connection limit.
            TrackerCounter::Channel {
                status_notification_tx,
            } => {
                let _ = status_notification_tx.send(ConnectionStatus::Closed);
            }
            // The held slot is released when this tracker drops.
            TrackerCounter::Shared { open, .. } => {
                open.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}
