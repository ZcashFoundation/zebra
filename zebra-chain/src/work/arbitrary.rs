use super::*;

use proptest::{collection::vec, prelude::*};

impl Arbitrary for equihash::Solution {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), equihash::SOLUTION_SIZE))
            .prop_map(|v| {
                let mut bytes = [0; equihash::SOLUTION_SIZE];
                bytes.copy_from_slice(v.as_slice());
                Self::Common(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
