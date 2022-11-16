//! Context of the current best block chain.

use std::sync::Arc;

use zebra_chain::{
    block::{Header, Height},
    parameters::POW_AVERAGING_WINDOW,
};

use crate::{
    request::HashOrHeight,
    service::{
        check::difficulty::POW_MEDIAN_BLOCK_SPAN, finalized_state::ZebraDb,
        non_finalized_state::Chain,
    },
};

/// Return the last block headers.
///
/// The number of headers is the constant `NEEDED_CONTEXT_BLOCKS`.
pub fn last_block_headers<C>(
    chain: Option<C>,
    db: &ZebraDb,
    height: Height,
) -> Option<Vec<Arc<Header>>>
where
    C: AsRef<Chain>,
{
    const NEEDED_CONTEXT_BLOCKS: usize = POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN;
    let mut headers = vec![];

    if height.0
        > NEEDED_CONTEXT_BLOCKS
            .try_into()
            .expect("number is small enough to always fit u32")
    {
        for i in 0..(NEEDED_CONTEXT_BLOCKS) {
            let i_signed: i32 = i
                .try_into()
                .expect("number is small enough to always fit i32");

            let res = chain
                .as_ref().map(|chain| chain.as_ref().non_finalized_nth_header(i))
                .or_else(|| Some(db.block_header(HashOrHeight::Height((height - i_signed).expect("can't fail as we are always in a tip height greater than NEEDED_CONTEXT_BLOCKS")))));

            if let Some(Some(header)) = res {
                headers.push(header.clone())
            }
        }

        Some(headers)
    } else {
        None
    }
}
