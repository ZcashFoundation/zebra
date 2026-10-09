use crate::{
    block::{Block, Header, MAX_BLOCK_BYTES},
    serialization::{
        CompactSizeMessage, SerializationError, ZcashDeserialize, ZcashDeserializeInto,
        ZcashSerialize,
    },
    work::equihash::{Solution, SOLUTION_SIZE},
};

use super::super::*;

/// Includes the 32-byte nonce.
const EQUIHASH_SOLUTION_BLOCK_OFFSET: usize = equihash::Solution::INPUT_LENGTH + 32;

/// Includes the 3-byte equihash length field.
const BLOCK_HEADER_LENGTH: usize = EQUIHASH_SOLUTION_BLOCK_OFFSET + 3 + equihash::SOLUTION_SIZE;

#[test]
fn equihash_solution_test_vectors() {
    let _init_guard = zebra_test::init();

    for block in zebra_test::vectors::BLOCKS.iter() {
        let solution_bytes = &block[EQUIHASH_SOLUTION_BLOCK_OFFSET..BLOCK_HEADER_LENGTH];

        let solution = solution_bytes
            .zcash_deserialize_into::<equihash::Solution>()
            .expect("Test vector EquihashSolution should deserialize");

        let mut data = Vec::new();
        solution
            .zcash_serialize(&mut data)
            .expect("Test vector EquihashSolution should serialize");

        assert_eq!(solution_bytes.len(), data.len());
        assert_eq!(solution_bytes, data.as_slice());
    }
}

#[test]
fn equihash_solution_test_vectors_are_valid() -> color_eyre::eyre::Result<()> {
    let _init_guard = zebra_test::init();

    for block in zebra_test::vectors::BLOCKS.iter() {
        let block =
            Block::zcash_deserialize(&block[..]).expect("block test vector should deserialize");

        assert_eq!(
            Solution::input(&block.header).as_slice(),
            &block.header.zcash_serialize_to_vec()?[..Solution::INPUT_LENGTH],
        );
        assert!(legacy_equihash_check(&block.header.solution, &block.header).is_ok());
        block.header.solution.check(&block.header)?;
    }

    Ok(())
}

/// The full-header serialization path, retained as a prefix regression and timing oracle.
fn legacy_equihash_check(
    solution: &Solution,
    header: &Header,
) -> Result<(), crate::work::equihash::Error> {
    let input = header.zcash_serialize_to_vec().unwrap();
    let bytes = match solution {
        Solution::Common(bytes) => bytes.as_slice(),
        Solution::Regtest(bytes) => bytes.as_slice(),
    };
    ::equihash::is_valid_solution(
        200,
        9,
        &input[..Solution::INPUT_LENGTH],
        header.nonce.as_ref(),
        bytes,
    )
    .map_err(Into::into)
}

#[test]
fn production_equihash_rejects_valid_regtest_proof() {
    let _init_guard = zebra_test::init();
    let header = *crate::block::genesis::regtest_genesis_block().header;
    let Solution::Regtest(solution) = header.solution else {
        panic!("the Regtest genesis fixture must contain a short solution");
    };
    ::equihash::is_valid_solution(
        48,
        5,
        &Solution::input(&header),
        header.nonce.as_ref(),
        &solution,
    )
    .expect("the fixture contains a valid Regtest proof");
    assert!(
        header.solution.check(&header).is_err(),
        "the network-independent production verifier must require Equihash (200, 9)",
    );
}

/// Replaces one packed 21-bit index without changing any other solution bits.
fn replace_solution_index(solution: &mut [u8; SOLUTION_SIZE], from: usize, to: usize) {
    for bit in 0..21 {
        let source_bit = from * 21 + bit;
        let target_bit = to * 21 + bit;
        let value = (solution[source_bit / 8] >> (7 - source_bit % 8)) & 1;
        let mask = 1 << (7 - target_bit % 8);
        solution[target_bit / 8] =
            (solution[target_bit / 8] & !mask) | (value << (7 - target_bit % 8));
    }
}

fn malformed_solutions(solution: Solution) -> [Solution; 6] {
    let Solution::Common(bytes) = solution else {
        panic!("historical Mainnet and Testnet vectors have 1344-byte solutions");
    };
    let mut bit_flip = bytes;
    bit_flip[SOLUTION_SIZE / 2] ^= 1;
    let mut duplicate = bytes;
    replace_solution_index(&mut duplicate, 0, 1);
    let mut reversed = bytes;
    // Preserve the first index while moving the second to the first slot.
    replace_solution_index(&mut reversed, 1, 0);
    for bit in 0..21 {
        let value = (bytes[bit / 8] >> (7 - bit % 8)) & 1;
        let target_bit = 21 + bit;
        let mask = 1 << (7 - target_bit % 8);
        reversed[target_bit / 8] =
            (reversed[target_bit / 8] & !mask) | (value << (7 - target_bit % 8));
    }
    [
        Solution::Common([0; SOLUTION_SIZE]),
        Solution::Common([0xff; SOLUTION_SIZE]),
        Solution::Common(bit_flip),
        Solution::Common(duplicate),
        Solution::Common(reversed),
        Solution::Regtest([0; crate::work::equihash::REGTEST_SOLUTION_SIZE]),
    ]
}

#[test]
fn equihash_fixed_prefix_and_malformed_solution_regression() {
    let _init_guard = zebra_test::init();

    for bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(&bytes[..]).unwrap();
        let header = *block.header;
        assert_eq!(
            Solution::input(&header).as_slice(),
            &bytes[..Solution::INPUT_LENGTH],
        );
        for version in [4, 5, 536_870_912, 0x7fff_ffff] {
            let mut changed = header;
            changed.version = version;
            assert_eq!(
                Solution::input(&changed).as_slice(),
                &changed.zcash_serialize_to_vec().unwrap()[..Solution::INPUT_LENGTH],
            );
        }
        for solution in malformed_solutions(header.solution) {
            assert!(solution.check(&header).is_err());
        }
        // `check` verifies its receiver, not the solution stored in the header.
        let mut changed = header;
        changed.solution = Solution::for_proposal();
        assert_eq!(Solution::input(&changed), Solution::input(&header));
        header.solution.check(&changed).unwrap();

        for field in 0..7 {
            let mut changed = header;
            match field {
                0 => changed.version ^= 1,
                1 => changed.previous_block_hash.0[0] ^= 1,
                2 => changed.merkle_root.0[0] ^= 1,
                3 => changed.commitment_bytes[0] ^= 1,
                4 => changed.time += chrono::Duration::seconds(1),
                5 => changed.difficulty_threshold.0 ^= 1,
                6 => changed.nonce[0] ^= 1,
                _ => unreachable!(),
            }
            assert!(changed.solution.check(&changed).is_err());
        }
    }
}

#[test]
fn equihash_fixed_prefix_preserves_serializer_invariants() {
    let _init_guard = zebra_test::init();
    let block = Block::zcash_deserialize(zebra_test::vectors::BLOCKS[0]).unwrap();
    let mut header = *block.header;

    for seconds in [0, i64::from(u32::MAX)] {
        header.time = chrono::DateTime::from_timestamp(seconds, 0).unwrap();
        assert_eq!(
            Solution::input(&header).as_slice(),
            &header.zcash_serialize_to_vec().unwrap()[..Solution::INPUT_LENGTH],
        );
    }
    for seconds in [-1, i64::from(u32::MAX) + 1] {
        header.time = chrono::DateTime::from_timestamp(seconds, 0).unwrap();
        assert!(std::panic::catch_unwind(|| Solution::input(&header)).is_err());
        assert!(std::panic::catch_unwind(|| header.zcash_serialize_to_vec()).is_err());
    }
    header = *block.header;
    for version in [0, 3, 0x8000_0000, u32::MAX] {
        header.version = version;
        assert!(std::panic::catch_unwind(|| Solution::input(&header)).is_err());
        assert!(header.zcash_serialize_to_vec().is_err());
    }
}

/// Compares full-header serialization with Zebra's fixed-prefix wrapper on identical proofs.
///
/// Run in release mode with `--ignored --nocapture`; optionally set `EQUIHASH_BENCH_ITERATIONS`.
#[test]
#[ignore = "manual full-header-versus-fixed-prefix performance measurement"]
#[allow(clippy::print_stdout)]
fn equihash_verification_benchmark() {
    use std::{hint::black_box, time::Instant};

    let _init_guard = zebra_test::init();
    let iterations: u32 = std::env::var("EQUIHASH_BENCH_ITERATIONS")
        .unwrap_or_else(|_| "1000".to_owned())
        .parse()
        .unwrap();
    assert!(iterations > 0);
    let block = Block::zcash_deserialize(zebra_test::vectors::BLOCKS[0]).unwrap();
    let header = *block.header;
    let malformed = malformed_solutions(header.solution);

    for (name, solution) in [
        ("valid", header.solution),
        ("invalid_bit_flip", malformed[2]),
        ("invalid_duplicate", malformed[3]),
        ("invalid_order", malformed[4]),
    ] {
        let expected = legacy_equihash_check(&solution, &header).is_ok();
        assert_eq!(solution.check(&header).is_ok(), expected);
        let variant_names = ["full_header_wrapper", "fixed_prefix_wrapper"];
        let mut samples = [[0u128; 2]; 7];
        for (round, timings) in samples.iter_mut().enumerate() {
            let order = if round % 2 == 0 { [0, 1] } else { [1, 0] };
            for variant in order {
                let start = Instant::now();
                for _ in 0..iterations {
                    let accepted = match variant {
                        0 => {
                            legacy_equihash_check(black_box(&solution), black_box(&header)).is_ok()
                        }
                        1 => black_box(&solution).check(black_box(&header)).is_ok(),
                        _ => unreachable!(),
                    };
                    black_box(accepted);
                }
                timings[variant] = start.elapsed().as_nanos();
            }
        }
        for (variant_index, variant) in variant_names.iter().enumerate() {
            let timings = samples.map(|sample| sample[variant_index]);
            for (sample, elapsed_ns) in timings.iter().enumerate() {
                println!(
                    "{name}/{variant}: sample={sample} iterations={iterations} \
                     elapsed_ns={elapsed_ns} ns/check={}",
                    elapsed_ns / u128::from(iterations),
                );
            }
            let mut sorted = timings;
            sorted.sort_unstable();
            println!(
                "{name}/{variant}: median_elapsed_ns={} iterations={iterations} \
                 median_ns/check={}",
                sorted[3],
                sorted[3] / u128::from(iterations),
            );
        }
    }
}

static EQUIHASH_SIZE_TESTS: &[usize] = &[
    0,
    1,
    SOLUTION_SIZE - 1,
    SOLUTION_SIZE,
    SOLUTION_SIZE + 1,
    (MAX_BLOCK_BYTES - 1) as usize,
    MAX_BLOCK_BYTES as usize,
];

#[test]
fn equihash_solution_size_field() {
    let _init_guard = zebra_test::init();

    for size in EQUIHASH_SIZE_TESTS.iter().copied() {
        let mut data = Vec::new();

        let size: CompactSizeMessage = size
            .try_into()
            .expect("test size fits in MAX_PROTOCOL_MESSAGE_LEN");
        size.zcash_serialize(&mut data)
            .expect("CompactSize should serialize");
        data.resize(data.len() + SOLUTION_SIZE, 0);

        let result = Solution::zcash_deserialize(data.as_slice());
        if size == SOLUTION_SIZE.try_into().unwrap() {
            result.expect("Correct size field in EquihashSolution should deserialize");
        } else {
            result.expect_err("Wrong size field in EquihashSolution should fail on deserialize");
        }
    }
}

#[test]
fn equihash_solution_rejects_oversize_compactsize_before_allocating() {
    let _init_guard = zebra_test::init();

    let mut data = Vec::new();
    let oversize: CompactSizeMessage = (SOLUTION_SIZE + 1)
        .try_into()
        .expect("fits in MAX_PROTOCOL_MESSAGE_LEN");
    oversize
        .zcash_serialize(&mut data)
        .expect("CompactSize should serialize");

    let err = Solution::zcash_deserialize(data.as_slice())
        .expect_err("oversize equihash CompactSize must fail to deserialize");

    // This is fragile, but the only current way to check if the deserializer
    // rejected the size before allocating.
    // If this fails, double check if the message error has not changed.
    assert!(
        matches!(
            err,
            SerializationError::Parse("incorrect equihash solution size"),
        ),
        "expected size-rejection Parse error, got: {err:?}",
    );
}
