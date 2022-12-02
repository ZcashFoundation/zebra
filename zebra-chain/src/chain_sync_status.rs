//! Defines method signatures for checking if the synchronizer is likely close to the network chain tip.

#[cfg(any(test, feature = "proptest-impl"))]
pub mod mock;

#[cfg(any(test, feature = "proptest-impl"))]
pub use mock::MockSyncStatus;

/// An interface for checking if the synchronization is likely close to the network chain tip.
pub trait ChainSyncStatus {
    /// Check if the synchronization is likely close to the network chain tip.
    fn is_close_to_tip(&self) -> bool;
}
