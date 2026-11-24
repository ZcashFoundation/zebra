//! Instruction-count guard for Orchard tree updates and large block decoding.

#![allow(missing_docs)]

use std::{hint::black_box, sync::Arc};

use gungraun::prelude::*;
use halo2::pasta::pallas;
use zebra_chain::{
    block::Block, orchard, parallel::tree::NoteCommitmentTrees, serialization::ZcashDeserialize,
};
use zebra_test::vectors::BLOCK_TESTNET_141042_BYTES;

fn orchard_commitments() -> Vec<pallas::Base> {
    // One block with 32 Orchard actions; deterministic valid field elements.
    let commitments: Vec<_> = (0..32).map(pallas::Base::from).collect();
    // The same update builds the lazy hash domains and tables outside the measured function.
    NoteCommitmentTrees::update_orchard_note_commitment_tree(
        Arc::new(orchard::tree::NoteCommitmentTree::default()),
        commitments.clone(),
    )
    .expect("32 commitments fit in the Orchard tree");
    commitments
}

#[library_benchmark]
#[bench::block_32(orchard_commitments())]
fn orchard_block_update(commitments: Vec<pallas::Base>) {
    let (tree, _) = NoteCommitmentTrees::update_orchard_note_commitment_tree(
        Arc::new(orchard::tree::NoteCommitmentTree::default()),
        black_box(commitments),
    )
    .expect("32 commitments fit in the Orchard tree");
    black_box(tree.root());
}

#[library_benchmark]
#[bench::testnet_141042(BLOCK_TESTNET_141042_BYTES.as_slice())]
fn deserialize_large_block(bytes: &[u8]) {
    black_box(Block::zcash_deserialize(black_box(bytes)).expect("test vector is a valid block"));
}

library_benchmark_group!(
    name = mining_chain,
    benchmarks = [orchard_block_update, deserialize_large_block]
);
main!(library_benchmark_groups = mining_chain);
