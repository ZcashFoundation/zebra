mod blocks;

use std::sync::Arc;

pub(crate) use blocks::BLOCKS;
use itertools::Itertools;

use crate::{block::Block, serialization::ZcashDeserializeInto};

// TODO: Move this to zebra-test.
pub fn valid_issuance_blocks() -> Vec<Arc<Block>> {
    BLOCKS
        .iter()
        .copied()
        .map(ZcashDeserializeInto::zcash_deserialize_into)
        .map(|result| result.map(Arc::new))
        .try_collect()
        .expect("hard-coded block data must deserialize successfully")
}
