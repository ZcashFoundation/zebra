use super::{difficulty::*, *};

use crate::block;

use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};

impl Arbitrary for equihash::Solution {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), equihash::SOLUTION_SIZE))
            .prop_map(|v| {
                let mut bytes = [0; equihash::SOLUTION_SIZE];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for CompactDifficulty {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 32))
            .prop_filter_map("zero CompactDifficulty values are invalid", |v| {
                let mut bytes = [0; 32];
                bytes.copy_from_slice(v.as_slice());
                if bytes == [0; 32] {
                    return None;
                }
                // In the Zcash protocol, a CompactDifficulty is generated using the difficulty
                // adjustment functions. Instead of using those functions, we make a random
                // ExpandedDifficulty, then convert it to a CompactDifficulty.
                ExpandedDifficulty::from_hash(&block::Hash(bytes))
                    .to_compact()
                    .into()
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
