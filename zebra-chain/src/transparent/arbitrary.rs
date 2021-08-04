use proptest::{arbitrary::any, collection::vec, prelude::*};

use crate::{block, LedgerState};

use super::{CoinbaseData, Input, OutPoint, Script, GENESIS_COINBASE_DATA};

impl Input {
    /// Construct a strategy for creating valid-ish vecs of Inputs.
    pub fn vec_strategy(ledger_state: LedgerState, max_size: usize) -> BoxedStrategy<Vec<Self>> {
        if ledger_state.has_coinbase {
            Self::arbitrary_with(Some(ledger_state.height))
                .prop_map(|input| vec![input])
                .boxed()
        } else {
            vec(Self::arbitrary_with(None), max_size).boxed()
        }
    }
}

impl Arbitrary for Input {
    type Parameters = Option<block::Height>;

    fn arbitrary_with(height: Self::Parameters) -> Self::Strategy {
        if let Some(height) = height {
            (vec(any::<u8>(), 0..95), any::<u32>())
                .prop_map(move |(data, sequence)| Input::Coinbase {
                    height,
                    data: if height == block::Height(0) {
                        CoinbaseData(GENESIS_COINBASE_DATA.to_vec())
                    } else {
                        CoinbaseData(data)
                    },
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
