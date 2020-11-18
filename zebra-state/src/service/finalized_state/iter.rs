//! An iterator for FinalizedState, in reverse chain order.

use std::sync::Arc;

use super::FinalizedState;

use zebra_chain::block::{self, Block};

pub struct Iter<'a> {
    pub(super) finalized_state: &'a FinalizedState,
    pub(super) height: Option<block::Height>,
}

impl Iterator for Iter<'_> {
    type Item = Arc<Block>;

    fn next(&mut self) -> Option<Self::Item> {
        let Iter {
            finalized_state,
            height,
        } = self;

        if let Some(height) = *height {
            if let Some(block) = finalized_state.block(height.into()) {
                self.height = height - 1;
                return Some(block);
            }
        }

        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl std::iter::FusedIterator for Iter<'_> {}

impl ExactSizeIterator for Iter<'_> {
    fn len(&self) -> usize {
        match (self.height, self.finalized_state.finalized_tip_height()) {
            (Some(height), Some(finalized_tip_height)) if (height <= finalized_tip_height) => {
                (height.0 + 1) as _
            }
            _ => 0,
        }
    }
}
