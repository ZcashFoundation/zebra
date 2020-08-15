use crate::merkle_tree::MerkleTreeRootHash;
use crate::parameters::Network;
use crate::work::{difficulty::CompactDifficulty, equihash};

use super::super::*;

use chrono::{TimeZone, Utc};
use proptest::{
    arbitrary::{any, Arbitrary},
    prelude::*,
};

impl Arbitrary for LightClientRootHash {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (any::<[u8; 32]>(), any::<Network>(), any::<BlockHeight>())
            .prop_map(|(light_client_root_bytes, network, block_height)| {
                LightClientRootHash::from_bytes(light_client_root_bytes, network, block_height)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for BlockHeader {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (
            // version is interpreted as i32 in the spec, so we are limited to i32::MAX here
            (4u32..(i32::MAX as u32)),
            any::<BlockHeaderHash>(),
            any::<MerkleTreeRootHash>(),
            any::<[u8; 32]>(),
            // time is interpreted as u32 in the spec, but rust timestamps are i64
            (0i64..(u32::MAX as i64)),
            any::<CompactDifficulty>(),
            any::<[u8; 32]>(),
            any::<equihash::Solution>(),
        )
            .prop_map(
                |(
                    version,
                    previous_block_hash,
                    merkle_root_hash,
                    light_client_root_bytes,
                    timestamp,
                    difficulty_threshold,
                    nonce,
                    solution,
                )| BlockHeader {
                    version,
                    previous_block_hash,
                    merkle_root_hash,
                    light_client_root_bytes,
                    time: Utc.timestamp(timestamp, 0),
                    difficulty_threshold,
                    nonce,
                    solution,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
