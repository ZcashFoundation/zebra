//! Counting active connections used by Zebra.
//!
//! These types can be used to count any kind of active resource.
//! But they are currently used to track the number of open connections.

use std::fmt;

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

    /// The channel used to send closed connection notifications.
    close_notification_tx: mpsc::UnboundedSender<ConnectionClosed>,

    /// The channel used to receive closed connection notifications.
    close_notification_rx: mpsc::UnboundedReceiver<ConnectionClosed>,
}

impl fmt::Debug for ActiveConnectionCounter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActiveConnectionCounter")
            .field("count", &self.count)
            .finish()
    }
}

impl ActiveConnectionCounter {
    /// Create and return a new active connection counter.
    pub fn new_counter() -> Self {
        // The number of items in this channel is bounded by the connection limit.
        let (close_notification_tx, close_notification_rx) = mpsc::unbounded_channel();

        Self {
            count: 0,
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
                "a peer connection was closed"
            );
        }

        trace!(
            open_connections = ?self.count,
            ?previous_connections,
            "updated active connection count"
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
}

impl fmt::Debug for ConnectionTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionTracker").finish()
    }
}

impl ConnectionTracker {
    /// Create and return a new active connection tracker, and add 1 to `counter`.
    ///
    /// When the returned tracker is dropped, `counter` will be notified, and decreased by 1.
    fn new(counter: &mut ActiveConnectionCounter) -> Self {
        counter.count += 1;

        debug!(open_connections = ?counter.count, "opening a new peer connection");

        Self {
            close_notification_tx: counter.close_notification_tx.clone(),
        }
    }
}

impl Drop for ConnectionTracker {
    /// Notifies the corresponding connection counter that the connection has closed.
    fn drop(&mut self) {
        // We ignore disconnected errors, because the receiver can be dropped
        // before some connections are dropped.
        //
        // TODO: This channel will be bounded by the connection limit (#1850, #1851, #2902).
        let _ = self.close_notification_tx.send(ConnectionClosed);
    }
}
