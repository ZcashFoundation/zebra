//! Counting active connections used by Zebra.
//!
//! These types can be used to count any kind of active resource.
//! But they are currently used to track the number of open connections.

use std::{fmt, sync::Arc};

use tokio::sync::mpsc;

/// A signal sent by a [`Connection`][1] when it closes.
///
/// Used to count the number of open connections.
///
/// [1]: crate::peer::Connection
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionClosed;

/// A counter for active connections.
///
/// Creates a [`ConnectionTracker`] to track each active connection.
/// When these trackers are dropped, the counter gets notified.
pub struct ActiveConnectionCounter {
    /// The number of active peers tracked using this counter.
    count: usize,

    /// The limit for this type of connection, for diagnostics only.
    /// The caller must enforce the limit by ignoring, delaying, or dropping connections.
    limit: usize,

    /// The label for this connection counter, typically its type.
    label: Arc<str>,

    /// The channel used to send closed connection notifications.
    close_notification_tx: mpsc::UnboundedSender<ConnectionClosed>,

    /// The channel used to receive closed connection notifications.
    close_notification_rx: mpsc::UnboundedReceiver<ConnectionClosed>,
}

impl fmt::Debug for ActiveConnectionCounter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActiveConnectionCounter")
            .field("label", &self.label)
            .field("count", &self.count)
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
        let (close_notification_tx, close_notification_rx) = mpsc::unbounded_channel();

        let label = label.to_string();

        Self {
            count: 0,
            limit,
            label: label.into(),
            close_notification_rx,
            close_notification_tx,
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
        while let Ok(ConnectionClosed) = self.close_notification_rx.try_recv() {
            self.count -= 1;

            debug!(
                open_connections = ?self.count,
                ?previous_connections,
                limit = ?self.limit,
                label = ?self.label,
                "a peer connection was closed",
            );
        }

        trace!(
            open_connections = ?self.count,
            ?previous_connections,
            limit = ?self.limit,
            label = ?self.label,
            "updated active connection count",
        );

        self.count
    }
}

/// A per-connection tracker.
///
/// [`ActiveConnectionCounter`] creates a tracker instance for each active connection.
/// When these trackers are dropped, the counter gets notified.
pub struct ConnectionTracker {
    /// The channel used to send closed connection notifications on drop.
    close_notification_tx: mpsc::UnboundedSender<ConnectionClosed>,

    /// The label for this connection counter, typically its type.
    label: Arc<str>,
}

impl fmt::Debug for ConnectionTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ConnectionTracker")
            .field(&self.label)
            .finish()
    }
}

impl ConnectionTracker {
    /// Create and return a new active connection tracker, and add 1 to `counter`.
    /// All connection trackers share a label with their connection counter.
    ///
    /// When the returned tracker is dropped, `counter` will be notified, and decreased by 1.
    fn new(counter: &mut ActiveConnectionCounter) -> Self {
        counter.count += 1;

        debug!(
            open_connections = ?counter.count,
            limit = ?counter.limit,
            label = ?counter.label,
            "opening a new peer connection",
        );

        Self {
            close_notification_tx: counter.close_notification_tx.clone(),
            label: counter.label.clone(),
        }
    }
}

impl Drop for ConnectionTracker {
    /// Notifies the corresponding connection counter that the connection has closed.
    fn drop(&mut self) {
        debug!(label = ?self.label, "closing a peer connection");

        // We ignore disconnected errors, because the receiver can be dropped
        // before some connections are dropped.
        // # Security
        //
        // This channel is actually bounded by the inbound and outbound connection limit.
        let _ = self.close_notification_tx.send(ConnectionClosed);
    }
}
