//! Shared [`tokio::sync::watch`] channel wrappers.
//!
//! These wrappers help use watch channels correctly.

use tokio::sync::watch;

/// Efficient access to state data via a [`tokio`] [`watch::Receiver`] channel,
/// while avoiding deadlocks.
///
/// Returns data from the most recent state,
/// regardless of how many times you call its methods.
///
/// Cloned instances provide identical state data.
///
/// # Correctness
///
/// To avoid deadlocks, see the correctness note on [`WatchReceiver::with_watch_data`].
///
/// # Note
///
/// If a lot of blocks are committed at the same time,
/// the watch chanel will skip some block updates,
/// even though those updates were committed to the state.
#[derive(Clone, Debug)]
pub struct WatchReceiver<T> {
    /// The receiver for the current state data.
    receiver: watch::Receiver<T>,
}

impl<T> WatchReceiver<T> {
    /// Create a new [`WatchReceiver`] from a watch channel receiver.
    pub fn new(receiver: watch::Receiver<T>) -> Self {
        Self { receiver }
    }
}

impl<T> WatchReceiver<T>
where
    T: Clone,
{
    /// Maps the current data `T` to `U` by applying a function to the watched value,
    /// while holding the receiver lock as briefly as possible.
    ///
    /// This helper method is a shorter way to borrow the value from the [`watch::Receiver`] and
    /// extract some information from it.
    ///
    /// # Performance
    ///
    /// A single read lock is acquired to clone `T`, and then released after the clone.
    /// To make this clone efficient, large or expensive `T` can be wrapped in an [`Arc`].
    /// (Or individual fields can be wrapped in an `Arc`.)
    ///
    /// # Correctness
    ///
    /// To prevent deadlocks:
    ///
    /// - `receiver.borrow()` should not be called before this method while in the same scope.
    ///
    /// It is important to avoid calling `borrow` more than once in the same scope, which
    /// effectively tries to acquire two read locks to the shared data in the watch channel. If
    /// that is done, there's a chance that the [`watch::Sender`] tries to send a value, which
    /// starts acquiring a write-lock, and prevents further read-locks from being acquired until
    /// the update is finished.
    ///
    /// What can happen in that scenario is:
    ///
    /// 1. The receiver manages to acquire a read-lock for the first `borrow`
    /// 2. The sender starts acquiring the write-lock
    /// 3. The receiver fails to acquire a read-lock for the second `borrow`
    ///
    /// Now both the sender and the receivers hang, because the sender won't release the lock until
    /// it can update the value, and the receiver won't release its first read-lock until it
    /// acquires the second read-lock and finishes what it's doing.
    pub fn with_watch_data<U, F>(&self, f: F) -> U
    where
        F: FnOnce(T) -> U,
    {
        let cloned_data = self.receiver.borrow().clone();

        f(cloned_data)
    }

    /// Calls [`watch::Receiver::changed`] and returns the result.
    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.receiver.changed().await
    }
}
