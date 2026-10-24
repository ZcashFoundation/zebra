//! Differential sigop counts and allocation witnesses for borrowed transparent bookkeeping.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
    sync::Arc,
};

use libzcash_script::ZcashScript;
use zcash_script::{opcode::PossiblyBad, script, script::Evaluable, Opcode};
use zebra_chain::{
    block::{Block, Height},
    parameters::NetworkUpgrade,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{LockTime, Transaction, UnminedTx},
    transparent::{Input, OutPoint, Output, Script},
};

use crate::{p2sh_input_sigop_count, p2sh_sigop_count, CachedFfiTransaction, Sigops};

struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

// SAFETY: every allocation and deallocation is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get().map(|n| n + 1)));
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measured<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|count| count.set(Some(0)));
    let result = black_box(f());
    let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    (result, allocations)
}

/// The old Zebra reconstruction path, independent of the borrowed script iterator.
fn old_legacy_sigops(tx: &Transaction) -> Result<u32, libzcash_script::Error> {
    let mut scripts: Vec<Vec<u8>> = tx
        .inputs()
        .into_iter()
        .map(|input| match &input {
            Input::PrevOut { unlock_script, .. } => unlock_script.as_raw_bytes().to_vec(),
            Input::Coinbase { .. } => input.coinbase_script().unwrap(),
        })
        .collect();
    scripts.extend(
        tx.outputs()
            .into_iter()
            .map(|output| output.lock_script.as_raw_bytes().to_vec()),
    );
    let interpreter = crate::get_interpreter(&|_, _| None, 0, true);
    Ok(scripts.into_iter().try_fold(0, |count, bytes| {
        interpreter
            .legacy_sigop_count_script(&script::Code(bytes))
            .map(|n| count + n)
    })?)
}

/// The old owned predicate, extraction, and accurate counter.
fn old_p2sh_sigops(input: &Input, spent: &Output) -> u32 {
    let Input::PrevOut { unlock_script, .. } = input else {
        return 0;
    };
    if !script::Code(spent.lock_script.as_raw_bytes().to_vec()).is_pay_to_script_hash() {
        return 0;
    }
    let code = script::Code(unlock_script.as_raw_bytes().to_vec());
    let mut last = None;
    for opcode in code.parse() {
        match opcode {
            Ok(PossiblyBad::Good(Opcode::PushValue(value))) => last = Some(value.value()),
            _ => return 0,
        }
    }
    last.map_or(0, |redeem| script::Code(redeem).sig_op_count(true))
}

fn input(bytes: &[u8]) -> Input {
    Input::PrevOut {
        outpoint: OutPoint {
            hash: [1; 32].into(),
            index: 0,
        },
        unlock_script: Script::new(bytes),
        sequence: u32::MAX,
    }
}

fn output(bytes: &[u8]) -> Output {
    Output {
        value: 1.try_into().unwrap(),
        lock_script: Script::new(bytes),
    }
}

fn transactions(inputs: Vec<Input>, outputs: Vec<Output>) -> [(Transaction, NetworkUpgrade); 3] {
    [
        (
            Transaction::test_v4(
                inputs.clone(),
                outputs.clone(),
                LockTime::unlocked(),
                Height(0),
            ),
            NetworkUpgrade::Canopy,
        ),
        (
            Transaction::test_v5(
                NetworkUpgrade::Nu5,
                inputs.clone(),
                outputs.clone(),
                LockTime::unlocked(),
                Height(0),
            ),
            NetworkUpgrade::Nu5,
        ),
        (
            Transaction::test_v6(
                NetworkUpgrade::Nu6_3,
                inputs,
                outputs,
                LockTime::unlocked(),
                Height(0),
            ),
            NetworkUpgrade::Nu6_3,
        ),
    ]
}

fn scripts() -> Vec<Vec<u8>> {
    let mut scripts = vec![
        vec![],
        vec![0xac, 0xad, 0xae, 0xaf],
        vec![0xab, 0xac, 0x7e, 0xad], // Disabled opcodes do not stop sigop counting.
        vec![0x01, 0xac, 0xad],       // Pushed sigops are data, not operations.
        vec![0xac, 0x4c],
        vec![0xac, 0x4d, 0xff],
        vec![0xac, 0x4e, 0xff, 0xff, 0xff, 0xff],
        vec![0xac, 0x02, 0xad], // Count stops at the truncated push.
        vec![0x50],             // OP_RESERVED is not a redeem-script data push.
    ];
    for opcode in 0u8..=255 {
        scripts.push(vec![opcode, 0xae, 0xaf, 0xac]);
    }
    let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(12000);
    for len in 0..256 {
        let mut bytes = vec![0; len];
        rand::Rng::fill_bytes(&mut rng, &mut bytes);
        scripts.push(bytes);
    }
    scripts
}

#[test]
fn borrowed_legacy_sigops_match_reconstruction() {
    let _init_guard = zebra_test::init();
    let scripts = scripts();
    let mut cases = Vec::from(transactions(
        scripts.iter().map(|bytes| input(bytes)).collect(),
        scripts.iter().map(|bytes| output(bytes)).collect(),
    ));
    cases.extend(transactions(vec![], vec![]));
    // The height contains CHECKSIG bytes that are pushed data, not independent opcodes.
    cases.extend(transactions(
        vec![Input::Coinbase {
            height: Height(0xacac),
            data: vec![0xac, 0xad, 0x52, 0xae],
            sequence: u32::MAX,
        }],
        vec![output(&[0xac])],
    ));
    for bytes in [
        &*zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES,
        &*zebra_test::vectors::BLOCK_TESTNET_GENESIS_BYTES,
    ] {
        let block: Block = bytes.zcash_deserialize_into().unwrap();
        cases.push(((*block.transactions[0]).clone(), NetworkUpgrade::Genesis));
    }

    for (tx, nu) in cases {
        let (expected, old_allocations) = measured(|| old_legacy_sigops(&tx));
        let (actual, allocations) = measured(|| tx.sigops());
        assert_eq!(actual, expected);
        if tx.transparent_bundle().is_some() {
            assert!(
                allocations < old_allocations,
                "native scripts must not be reconstructed"
            );
        }

        let bytes = tx.zcash_serialize_to_vec().unwrap();
        let primitive = zcash_primitives::transaction::Transaction::read(
            bytes.as_slice(),
            tx.consensus_branch_id(),
        )
        .unwrap();
        assert_eq!(primitive.sigops(), expected);
        let tx = Arc::new(tx);
        assert_eq!(UnminedTx::from(tx.clone()).sigops(), expected);
        if tx.version() >= 4 {
            let previous = vec![output(&[]); tx.inputs().len()];
            let cached = CachedFfiTransaction::new(tx, Arc::new(previous), nu).unwrap();
            assert_eq!(cached.sigops(), expected);
        }
    }
}

#[test]
fn borrowed_p2sh_sigops_match_parser() {
    let _init_guard = zebra_test::init();
    let mut canonical = vec![0xa9, 0x14];
    canonical.extend([0xac; 20]);
    canonical.push(0x87);
    let mut locks = scripts();
    locks.push(canonical.clone());
    for prefix in [vec![0x4c, 20], vec![0x4d, 20, 0], vec![0x4e, 20, 0, 0, 0]] {
        let mut lock = vec![0xa9];
        lock.extend(prefix);
        lock.extend([0xac; 20]);
        lock.push(0x87);
        locks.push(lock);
    }
    for len in 0..canonical.len() {
        locks.push(canonical[..len].to_vec());
    }
    for index in 0..canonical.len() {
        for byte in 0..=255 {
            let mut lock = canonical.clone();
            lock[index] = byte;
            locks.push(lock);
        }
    }
    let mut extra = canonical.clone();
    extra.push(0x51);
    locks.push(extra);

    // One redeem-script sigop makes the count itself a differential predicate witness.
    let one_sigop = input(&[1, 0xac]);
    for lock in locks {
        let spent = output(&lock);
        let expected = old_p2sh_sigops(&one_sigop, &spent);
        let (actual, allocations) = measured(|| p2sh_input_sigop_count(&one_sigop, &spent));
        assert_eq!(actual, expected, "lock script: {lock:02x?}");
        if expected == 0 {
            assert_eq!(allocations, 0, "non-P2SH classification must not allocate");
        }
    }

    let mut unlocks = scripts();
    for redeem in scripts() {
        // Include all nonminimal push encodings of the redeem script.
        let length = u32::try_from(redeem.len()).unwrap();
        let bytes = length.to_le_bytes();
        let mut prefixes = vec![
            vec![0x4d, bytes[0], bytes[1]],
            vec![0x4e, bytes[0], bytes[1], 0, 0],
        ];
        if length <= 255 {
            prefixes.push(vec![0x4c, bytes[0]]);
        }
        if length <= 75 {
            prefixes.push(vec![bytes[0]]);
        }
        for mut prefix in prefixes {
            prefix.extend(&redeem);
            unlocks.push(prefix);
        }
    }
    let inputs: Vec<_> = unlocks.iter().map(|bytes| input(bytes)).collect();
    let spent: Vec<_> = (0..inputs.len())
        .map(|index| output(if index % 2 == 0 { &canonical } else { &[0x51] }))
        .collect();
    let expected: u32 = inputs
        .iter()
        .zip(&spent)
        .map(|(i, o)| old_p2sh_sigops(i, o))
        .sum();
    for (tx, nu) in transactions(inputs, vec![]) {
        assert_eq!(p2sh_sigop_count(&tx, &spent), expected);
        let cached = CachedFfiTransaction::new(Arc::new(tx), Arc::new(spent.clone()), nu).unwrap();
        assert_eq!(cached.p2sh_sigops(), expected);
    }
}

#[test]
fn p2sh_sigops_reject_misaligned_spent_outputs_in_release() {
    for (tx, _) in transactions(vec![input(&[1, 0xac])], vec![]) {
        for spent in [vec![], vec![output(&[]); 2]] {
            assert!(std::panic::catch_unwind(|| p2sh_sigop_count(&tx, &spent)).is_err());
        }
    }
}
