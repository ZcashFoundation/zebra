#![allow(clippy::unwrap_in_result)]

mod prop;
mod vectors;

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
};

use zcash_transparent::{address::TransparentAddress, bundle::TxOut};

use crate::parameters::Network;

use super::{Address, Output};

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
