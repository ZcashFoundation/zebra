//! Hard-coded tests for recent sync lengths.

use super::super::RecentSyncLengths;

#[test]
fn recent_sync_lengths_are_initially_empty() {
    let (_recent_sync_lengths, receiver) = RecentSyncLengths::new(None);

    assert_eq!(receiver.borrow().len(), 0);
}
