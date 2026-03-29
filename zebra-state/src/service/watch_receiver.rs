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
/// the watch channel will skip some block updates,
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
    /// Does not mark the watched data as seen.
    ///
    /// # Performance
    ///
    /// A single read lock is acquired to clone `T`, and then released after the
    /// clone. To make this clone efficient, large or expensive `T` can be
    /// wrapped in an [`std::sync::Arc`]. (Or individual fields can be wrapped
    /// in an [`std::sync::Arc`].)
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
        // Make sure that the borrow's watch channel read lock
        // is dropped before the closure is executed.
        //
        // Without this change, an eager reader can repeatedly block the channel writer.
        // This seems to happen easily in RPC & ReadStateService futures.
        // (For example, when lightwalletd syncs from Zebra, while Zebra syncs from peers.)
        let cloned_data = self.cloned_watch_data();

        f(cloned_data)
    }

    /// Calls the provided closure with the watch data in the channel
    /// and returns the output.
    ///
    /// Does not mark the watched data as seen.
    ///
    /// The closure provided to this method will hold a read lock,
    /// callers are expected to ensure any closures they provide
    /// will promptly drop the read lock.
    pub fn borrow_mapped<U: 'static, F>(&self, f: F) -> U
    where
        F: FnOnce(watch::Ref<T>) -> U,
    {
        f(self.receiver.borrow())
    }

    /// Returns a clone of the watch data in the channel.
    /// Cloning the watched data helps avoid deadlocks.
    ///
    /// Does not mark the watched data as seen.
    ///
    /// See `with_watch_data()` for details.
    pub fn cloned_watch_data(&self) -> T {
        self.receiver.borrow().clone()
    }

    /// Calls [`watch::Receiver::changed()`] and returns the result.
    /// Returns when the inner value has been updated, even if the old and new values are equal.
    ///
    /// Marks the watched data as seen.
    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.receiver.changed().await
    }

    /// Calls [`watch::Receiver::has_changed()`] and returns the result.
    /// Returns `true` when the inner value has been updated, even if the old and new values are equal.
    ///
    /// Does not mark the watched data as seen.
    pub fn has_changed(&self) -> Result<bool, watch::error::RecvError> {
        self.receiver.has_changed()
    }

    /// Marks the watched data as seen.
    pub fn mark_as_seen(&mut self) {
        self.receiver.borrow_and_update();
    }

    /// Marks the watched data as unseen.
    /// Calls [`watch::Receiver::mark_changed()`].
    pub fn mark_changed(&mut self) {
        self.receiver.mark_changed();
    }
}
