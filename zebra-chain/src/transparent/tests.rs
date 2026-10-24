#![allow(clippy::unwrap_in_result)]

mod prop;
mod vectors;

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::HashMap,
    hint::black_box,
};

use zcash_transparent::{address::TransparentAddress, bundle::TxOut};

use crate::{
    block::{Block, Height},
    parameters::Network,
    transaction::{self, Transaction},
};

use super::{
    new_ordered_outputs_with_height, new_transaction_ordered_outputs, Address, OrderedUtxo,
    OutPoint, Output,
};

struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

fn record_allocation() {
    let _ = ALLOCATIONS.try_with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current + 1));
        }
    });
}

// Test-only: forward allocator contracts unchanged, counting calls on the current thread.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        System.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation();
        System.realloc(ptr, layout, new_size)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// Compare with the owned conversion and solver, and require allocation-free extraction.
fn assert_output_address(output: &Output, network: &Network) -> Option<Address> {
    let expected =
        TxOut::try_from(output)
            .unwrap()
            .recipient_address()
            .map(|address| match address {
                TransparentAddress::PublicKeyHash(hash) => {
                    Address::from_pub_key_hash(network.t_addr_kind(), hash)
                }
                TransparentAddress::ScriptHash(hash) => {
                    Address::from_script_hash(network.t_addr_kind(), hash)
                }
            });

    ALLOCATIONS.with(|count| count.set(Some(0)));
    let actual = black_box(output).address(black_box(network));
    let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert_eq!(actual, expected, "{output:?} on {network:?}");
    assert_eq!(allocations, 0, "address extraction must not allocate");
    actual
}

/// Compare the old per-transaction construction and count only destination-map/script allocations.
fn assert_ordered_outputs(block: &Block, height: Height, hashes: &[transaction::Hash]) {
    let mut expected = HashMap::new();
    let mut script_allocations = 0;
    for (tx_index, (tx, &hash)) in block.transactions.iter().zip(hashes).enumerate() {
        let mut transaction_outputs = HashMap::new();
        let mut transaction_scripts = 0;
        for (index, output) in tx.outputs().iter().cloned().enumerate() {
            transaction_scripts += usize::from(!output.lock_script.as_raw_bytes().is_empty());
            transaction_outputs.insert(
                OutPoint {
                    hash,
                    index: index.try_into().unwrap(),
                },
                OrderedUtxo::new(output, height, tx_index),
            );
        }

        script_allocations += transaction_scripts;
        ALLOCATIONS.with(|count| count.set(Some(0)));
        let actual = new_transaction_ordered_outputs(black_box(tx), hash, tx_index, height);
        let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
        assert_eq!(actual, transaction_outputs);
        assert_eq!(
            allocations,
            transaction_scripts + usize::from(!actual.is_empty()),
            "only the destination map and one copy of each nonempty script may allocate",
        );
        expected.extend(transaction_outputs);
    }

    ALLOCATIONS.with(|count| count.set(Some(0)));
    let actual = new_ordered_outputs_with_height(black_box(block), height, black_box(hashes));
    let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert_eq!(actual, expected);
    assert_eq!(
        allocations,
        script_allocations + usize::from(!actual.is_empty()),
        "only the destination map and one copy of each nonempty script may allocate",
    );
}

/// Build V4, V5, and V6 transactions without transparent bundles.
fn ordered_output_transactions() -> [Transaction; 3] {
    use crate::{parameters::NetworkUpgrade, transaction::LockTime};

    [
        Transaction::test_v4(Vec::new(), Vec::new(), LockTime::unlocked(), Height(0)),
        Transaction::test_v5(
            NetworkUpgrade::Nu5,
            Vec::new(),
            Vec::new(),
            LockTime::unlocked(),
            Height(0),
        ),
        Transaction::test_v6(
            NetworkUpgrade::Nu6_3,
            Vec::new(),
            Vec::new(),
            LockTime::unlocked(),
            Height(0),
        ),
    ]
}
