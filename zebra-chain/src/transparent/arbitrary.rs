use proptest::{collection::vec, prelude::*};
use zcash_transparent::coinbase::MinerData;

use crate::{
    block,
    parameters::NetworkKind,
    serialization::TrustedPreallocate,
    transparent::{Output, OutputIndex},
    LedgerState,
};

use super::{serialize::GENESIS_COINBASE_SCRIPT_SIG, Address, Input, OutPoint, Script};

impl Input {
    /// Construct a strategy for creating valid-ish vecs of Inputs.
    pub fn vec_strategy(ledger_state: &LedgerState, max_size: usize) -> BoxedStrategy<Vec<Self>> {
        if ledger_state.has_coinbase {
            Self::arbitrary_with(Some(ledger_state.height))
                .prop_map(|input| vec![input])
                .boxed()
        } else {
            vec(Self::arbitrary_with(None), 1..=max_size).boxed()
        }
    }
}

impl Arbitrary for Input {
    type Parameters = Option<block::Height>;

    fn arbitrary_with(height: Self::Parameters) -> Self::Strategy {
        if let Some(height) = height {
            (vec(any::<u8>(), 1..95), any::<u32>())
                .prop_map(move |(data, sequence)| Input::Coinbase {
                    height,
                    data: if height == block::Height(0) {
                        MinerData::try_from(GENESIS_COINBASE_SCRIPT_SIG.as_ref())
                            .expect("genesis coinbase data is valid")
                    } else {
                        MinerData::try_from(data.as_ref()).expect("coinbase data is valid")
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

impl Arbitrary for Address {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        any::<(bool, bool, [u8; 20])>()
            .prop_map(|(is_mainnet, is_p2pkh, hash_bytes)| {
                let network = if is_mainnet {
                    NetworkKind::Mainnet
                } else {
                    NetworkKind::Testnet
                };

                if is_p2pkh {
                    Address::from_pub_key_hash(network, hash_bytes)
                } else {
                    Address::from_script_hash(network, hash_bytes)
                }
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for OutputIndex {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (0..=Output::max_allocation())
            .prop_map(OutputIndex::from_u64)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
