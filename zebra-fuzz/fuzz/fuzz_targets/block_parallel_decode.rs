#![no_main]
//! Differential fuzz target for the parallel block decoder.
//!
//! [`Block::zcash_deserialize_parallel`] decodes blocks with 64 or more transactions in parallel.
//! It scans the transaction layout to find where each transaction ends, and decodes batches of
//! transactions at once. It must give exactly the serial decoder's block or error, and consume
//! the same bytes, for any input.
//!
//! Arbitrary bytes rarely form a header followed by 64 transactions, so the first input byte
//! selects how the block is built:
//!
//! * the rest of the input is the block;
//! * the rest of the input is a transaction, and the block has 64 to 252 copies of it, which
//!   compares the layout scan with the decoder for arbitrary transaction bytes;
//! * the rest of the input is split in two: the block has copies of the first part, followed
//!   by the second part.
//!
//! Only panics, aborts, and differences between the two decoders are bugs.

use libfuzzer_sys::fuzz_target;

use zebra_chain::{
    block::Block,
    serialization::{ZcashDeserialize, ZcashSerialize},
};

/// The bytes of a block header with an all-zero body and a Mainnet-sized Equihash solution.
fn header_bytes() -> Vec<u8> {
    let mut header = vec![4, 0, 0, 0];
    // Previous block hash, Merkle root, block commitments, time, difficulty and nonce.
    header.resize(header.len() + 32 * 3 + 4 + 4 + 32, 0);
    // The solution's CompactSize length, 1344, and the solution.
    header.extend_from_slice(&[0xfd, 0x40, 0x05]);
    header.resize(header.len() + 1344, 0);
    header
}

/// Builds the block bytes that `data` describes.
fn block_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let (&mode, rest) = data.split_first()?;

    match mode % 3 {
        0 => Some(rest.to_vec()),
        1 | 2 => {
            let (&count, rest) = rest.split_first()?;
            // 64 to 252 transactions, which is a one-byte CompactSize.
            let count = 64 + usize::from(count) % (252 - 64 + 1);

            let (repeated, last) = if mode % 3 == 1 {
                (rest, &[][..])
            } else {
                let (&split, rest) = rest.split_first()?;
                rest.split_at(usize::from(split).min(rest.len()))
            };

            let mut bytes = header_bytes();
            bytes.push(u8::try_from(count).expect("the count is at most 252"));
            for _ in 0..count - usize::from(!last.is_empty()) {
                bytes.extend_from_slice(repeated);
            }
            bytes.extend_from_slice(last);
            Some(bytes)
        }
        _ => unreachable!("the mode is reduced modulo 3"),
    }
}

fuzz_target!(|data: &[u8]| {
    let Some(bytes) = block_bytes(data) else {
        return;
    };

    let mut serial_bytes = bytes.as_slice();
    let mut parallel_bytes = bytes.as_slice();
    let serial = Block::zcash_deserialize(&mut serial_bytes);
    let parallel = Block::zcash_deserialize_parallel(&mut parallel_bytes);

    assert_eq!(
        serial_bytes.len(),
        parallel_bytes.len(),
        "the decoders consume different numbers of bytes"
    );

    match (serial, parallel) {
        (Ok(serial), Ok(parallel)) => {
            // Block equality only compares transaction IDs, so compare the whole encoding.
            assert_eq!(
                serial
                    .zcash_serialize_to_vec()
                    .expect("a Vec writer can't fail"),
                parallel
                    .zcash_serialize_to_vec()
                    .expect("a Vec writer can't fail"),
                "the decoders give different blocks"
            );
            for (serial, parallel) in serial.transactions.iter().zip(&parallel.transactions) {
                assert_eq!(serial.hash(), parallel.hash());
                assert_eq!(serial.auth_digest(), parallel.auth_digest());
            }
        }
        (Err(serial), Err(parallel)) => {
            assert_eq!(
                format!("{serial:?}"),
                format!("{parallel:?}"),
                "the decoders give different errors"
            );
        }
        (serial, parallel) => {
            panic!("only one decoder accepts the block: {serial:?} / {parallel:?}")
        }
    }
});
