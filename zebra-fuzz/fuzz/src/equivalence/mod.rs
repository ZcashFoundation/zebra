//! Backend verification oracles, not full transaction-consensus validation.
//!
//! Adapted from ZCG #332's batch-equivalence suite. Input is a concatenated
//! transaction stream followed by one control byte. Both targets use the same
//! bounded parser; transparent prevouts are absent, so transactions spending
//! transparent inputs are excluded from signature comparisons.

use std::{io::Cursor, sync::Arc};

use rand::{rngs::StdRng, SeedableRng};
use zebra_chain::{
    serialization::ZcashDeserialize,
    transaction::{HashType, SigHash, Transaction},
};

pub mod orchard;
pub mod tower;

/// Bound parsing and cryptographic work independently of libFuzzer's settings.
pub const MAX_INPUT_BYTES: usize = 2_000_000;
pub const MAX_TRANSACTIONS: usize = 32;
pub const MAX_ITEMS: usize = 32;

pub fn transactions(data: &[u8]) -> Vec<Transaction> {
    if data.len() > MAX_INPUT_BYTES {
        return Vec::new();
    }
    let mut cursor = Cursor::new(data);
    let mut txs = Vec::new();
    // The cursor only traverses this bounded slice, so its position fits usize.
    while (cursor.position() as usize) < data.len() && txs.len() < MAX_TRANSACTIONS {
        match Transaction::zcash_deserialize(&mut cursor) {
            Ok(tx) => txs.push(tx),
            Err(_) => break,
        }
    }
    txs
}

pub fn sighash(tx: &Transaction) -> Option<SigHash> {
    if !tx.inputs().is_empty() {
        return None;
    }
    let nu = tx.network_upgrade()?;
    let hasher = tx.sighasher(nu, Arc::new(Vec::new())).ok()?;
    Some(hasher.sighash(HashType::ALL, None))
}

/// FNV-1a: stable across processes; the RNG is for reproducibility, not secrecy.
pub fn seed(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

pub fn rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}

/// A shared batch rejection can legitimately reject a valid neighbour. Only
/// acceptance of an individually invalid item is a soundness disagreement.
pub fn assert_no_false_accept(batch: &[bool], single: &[bool]) {
    assert_eq!(batch.len(), single.len(), "one verdict per submitted item");
    for (i, (&b, &s)) in batch.iter().zip(single).enumerate() {
        assert!(!b || s, "batch false accept at item {i}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_rejection_is_not_a_false_accept() {
        assert_no_false_accept(&[false, false], &[true, false]);
    }

    #[test]
    #[should_panic(expected = "batch false accept at item 1")]
    fn false_accept_is_detected_per_item() {
        assert_no_false_accept(&[false, true], &[false, false]);
    }

    #[test]
    #[should_panic(expected = "one verdict per submitted item")]
    fn missing_verdict_cannot_pass() {
        assert_no_false_accept(&[], &[true]);
    }

    #[test]
    fn input_is_bounded() {
        assert!(transactions(&[]).is_empty());
        assert!(transactions(&[0xff; 16]).is_empty());
        assert!(transactions(&vec![0; MAX_INPUT_BYTES + 1]).is_empty());
        assert_eq!(seed(b"same"), seed(b"same"));
        assert_ne!(seed(b"same"), seed(b"different"));
    }
}
