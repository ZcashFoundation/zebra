use proptest::{arbitrary::any, collection::vec, prelude::*};

use crate::block;

use super::{CoinbaseData, Input, OutPoint, Script};

impl Input {
    /// Construct a strategy for creating validish vecs of Inputs.
    pub fn vec_strategy(height: block::Height, max_size: usize) -> BoxedStrategy<Vec<Self>> {
        (0..max_size)
            .prop_flat_map(move |count| {
                let mut inputs = vec![];
                for ind in 0..count {
                    if ind == 0 {
                        inputs.push(Self::arbitrary_with(Some(height)))
                    } else {
                        inputs.push(Self::arbitrary_with(None))
                    }
                }
                inputs
            })
            .boxed()
    }
}

impl Arbitrary for Input {
    type Parameters = Option<block::Height>;

    fn arbitrary_with(height: Self::Parameters) -> Self::Strategy {
        if let Some(height) = height {
            (vec(any::<u8>(), 0..95), any::<u32>())
                .prop_map(move |(data, sequence)| Input::Coinbase {
                    height,
                    data: CoinbaseData(data),
                    sequence,
                })
                .boxed()
        } else {
            (any::<OutPoint>(), any::<Script>(), any::<u32>())
                .prop_map(|(outpoint, unlock_script, sequence)| Input::PrevOut {
                    outpoint,
                    unlock_script,
                    sequence,
                })
                .boxed()
        }
    }

    type Strategy = BoxedStrategy<Self>;
}
