//! Iterators for blocks in the non-finalized and finalized state.

use std::sync::Arc;

use zebra_chain::block::{self, Block};

use crate::{service::StateService, HashOrHeight};

/// Iterator for state blocks.
///
/// Starts at any block in any non-finalized or finalized chain,
/// and iterates in reverse height order. (Towards the genesis block.)
pub(crate) struct Iter<'a> {
    pub(super) service: &'a StateService,
    pub(super) state: IterState,
}

pub(super) enum IterState {
    NonFinalized(block::Hash),
    Finalized(block::Height),
    Finished,
}

impl Iter<'_> {
    fn next_non_finalized_block(&mut self) -> Option<Arc<Block>> {
        let Iter { service, state } = self;

        let hash = match state {
            IterState::NonFinalized(hash) => *hash,
            IterState::Finalized(_) | IterState::Finished => unreachable!(),
        };

        if let Some(block) = service.mem.any_block_by_hash(hash) {
            let hash = block.header.previous_block_hash;
            self.state = IterState::NonFinalized(hash);
            Some(block)
        } else {
            None
        }
    }

    #[allow(clippy::unwrap_in_result)]
    fn next_finalized_block(&mut self) -> Option<Arc<Block>> {
        let Iter { service, state } = self;

        let hash_or_height: HashOrHeight = match *state {
            IterState::Finalized(height) => height.into(),
            IterState::NonFinalized(hash) => hash.into(),
            IterState::Finished => unreachable!(),
        };

        if let Some(block) = service.disk.db().block(hash_or_height) {
            let height = block
                .coinbase_height()
                .expect("valid blocks have a coinbase height");

            if let Some(next_height) = height - 1 {
                self.state = IterState::Finalized(next_height);
            } else {
                self.state = IterState::Finished;
            }

            Some(block)
        } else {
            self.state = IterState::Finished;
            None
        }
    }
}

impl Iterator for Iter<'_> {
    type Item = Arc<Block>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.state {
            IterState::NonFinalized(_) => self
                .next_non_finalized_block()
                .or_else(|| self.next_finalized_block()),
            IterState::Finalized(_) => self.next_finalized_block(),
            IterState::Finished => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl std::iter::FusedIterator for Iter<'_> {}

impl ExactSizeIterator for Iter<'_> {
    fn len(&self) -> usize {
        match self.state {
            IterState::NonFinalized(hash) => self
                .service
                .any_height_by_hash(hash)
                .map(|height| (height.0 + 1) as _)
                .unwrap_or(0),
            IterState::Finalized(height) => (height.0 + 1) as _,
            IterState::Finished => 0,
        }
    }
}
