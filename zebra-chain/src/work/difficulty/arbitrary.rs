use super::*;

use proptest::{collection::vec, prelude::*};

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
                Some(ExpandedDifficulty::from_hash(&block::Hash(bytes)).to_compact())
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for ExpandedDifficulty {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        any::<CompactDifficulty>()
            .prop_map(|d| {
                // In the Zcash protocol, an ExpandedDifficulty is converted from a CompactDifficulty,
                // or generated using the difficulty adjustment functions. We use the conversion in
                // our proptest strategy.
                d.to_expanded()
                    .expect("arbitrary CompactDifficulty is valid")
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Work {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        // In the Zcash protocol, a Work is converted from an ExpandedDifficulty.
        // But some randomised difficulties are impractically large, and will
        // never appear in any real-world block. So we just use a random Work value.
        (1..u128::MAX).prop_map(Work).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for PartialCumulativeWork {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        // In Zebra's implementation, a PartialCumulativeWork is the sum of 0..100 Work values.
        // But our Work values are randomised, rather than being derived from real-world
        // difficulties. So we don't need to sum multiple Work values here.
        (any::<Work>())
            .prop_map(PartialCumulativeWork::from)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
