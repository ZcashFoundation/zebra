use std::sync::Arc;

use crate::parameters::Network;
use crate::work::{difficulty::CompactDifficulty, equihash};

use super::*;

use crate::LedgerState;
use chrono::{TimeZone, Utc};
use proptest::{
    arbitrary::{any, Arbitrary},
    prelude::*,
};

impl Arbitrary for Block {
    type Parameters = LedgerState;

    fn arbitrary_with(ledger_state: Self::Parameters) -> Self::Strategy {
        let transactions_strategy = Transaction::vec_strategy(ledger_state, 2);

        (any::<Header>(), transactions_strategy)
            .prop_map(|(header, transactions)| Self {
                header,
                transactions,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Block {
    pub fn partial_chain_strategy(
        init: LedgerState,
        count: usize,
    ) -> BoxedStrategy<Vec<Arc<Self>>> {
        let mut current = init;
        let mut vec = Vec::with_capacity(count);
        for _ in 0..count {
            vec.push(Block::arbitrary_with(current).prop_map(Arc::new));
            current.tip_height.0 += 1;
        }

        vec.boxed()
    }
}

impl Arbitrary for RootHash {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (any::<[u8; 32]>(), any::<Network>(), any::<Height>())
            .prop_map(|(root_bytes, network, block_height)| {
                RootHash::from_bytes(root_bytes, network, block_height)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Header {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (
            // version is interpreted as i32 in the spec, so we are limited to i32::MAX here
            (4u32..(i32::MAX as u32)),
            any::<Hash>(),
            any::<merkle::Root>(),
            any::<[u8; 32]>(),
            // time is interpreted as u32 in the spec, but rust timestamps are i64
            (0i64..(u32::MAX as i64)),
            any::<[u8; 32]>(),
            any::<equihash::Solution>(),
        )
            .prop_map(
                |(
                    version,
                    previous_block_hash,
                    merkle_root_hash,
                    root_bytes,
                    timestamp,
                    nonce,
                    solution,
                )| Header {
                    version,
                    previous_block_hash,
                    merkle_root: merkle_root_hash,
                    root_bytes,
                    time: Utc.timestamp(timestamp, 0),
                    // TODO: replace with `ExpandedDifficulty.to_compact` when that method is implemented
                    difficulty_threshold: CompactDifficulty(545259519),
                    nonce,
                    solution,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
