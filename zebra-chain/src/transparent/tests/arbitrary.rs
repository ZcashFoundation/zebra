use proptest::{arbitrary::any, collection::vec, prelude::*};

use crate::block;

use super::super::{CoinbaseData, Input, OutPoint, Script};

impl Arbitrary for Input {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        prop_oneof![
            (any::<OutPoint>(), any::<Script>(), any::<u32>())
                .prop_map(|(outpoint, unlock_script, sequence)| {
                    Input::PrevOut {
                        outpoint,
                        unlock_script,
                        sequence,
                    }
                })
                .boxed(),
            (
                any::<block::Height>(),
                vec(any::<u8>(), 0..95),
                any::<u32>()
            )
                .prop_map(|(height, data, sequence)| {
                    Input::Coinbase {
                        height,
                        data: CoinbaseData(data),
                        sequence,
                    }
                })
                .boxed(),
        ]
        .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
