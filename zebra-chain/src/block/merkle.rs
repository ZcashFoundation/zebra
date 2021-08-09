//! The Bitcoin-inherited Merkle tree of transactions.
#![allow(clippy::unit_arg)]

use std::{fmt, io::Write};

#[cfg(any(any(test, feature = "proptest-impl"), feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use crate::serialization::sha256d;
use crate::transaction::{self, Transaction};

/// The root of the Bitcoin-inherited transaction Merkle tree, binding the
/// block header to the transactions in the block.
///
/// Note: for V5-onward transactions it does not bind to authorizing data
/// (signature and proofs) which makes it non-malleable [ZIP-244].
///
/// Note that because of a flaw in Bitcoin's design, the `merkle_root` does
/// not always precisely bind the contents of the block (CVE-2012-2459). It
/// is sometimes possible for an attacker to create multiple distinct sets of
/// transactions with the same Merkle root, although only one set will be
/// valid.
///
/// # Malleability
///
/// The Bitcoin source code contains the following note:
///
/// > WARNING! If you're reading this because you're learning about crypto
/// > and/or designing a new system that will use merkle trees, keep in mind
/// > that the following merkle tree algorithm has a serious flaw related to
/// > duplicate txids, resulting in a vulnerability (CVE-2012-2459).
/// > The reason is that if the number of hashes in the list at a given time
/// > is odd, the last one is duplicated before computing the next level (which
/// > is unusual in Merkle trees). This results in certain sequences of
/// > transactions leading to the same merkle root. For example, these two
/// > trees:
/// >
/// > ```ascii
/// >              A               A
/// >            /  \            /   \
/// >          B     C         B       C
/// >         / \    |        / \     / \
/// >        D   E   F       D   E   F   F
/// >       / \ / \ / \     / \ / \ / \ / \
/// >       1 2 3 4 5 6     1 2 3 4 5 6 5 6
/// > ```
/// >
/// > for transaction lists [1,2,3,4,5,6] and [1,2,3,4,5,6,5,6] (where 5 and
/// > 6 are repeated) result in the same root hash A (because the hash of both
/// > of (F) and (F,F) is C).
/// >
/// > The vulnerability results from being able to send a block with such a
/// > transaction list, with the same merkle root, and the same block hash as
/// > the original without duplication, resulting in failed validation. If the
/// > receiving node proceeds to mark that block as permanently invalid
/// > however, it will fail to accept further unmodified (and thus potentially
/// > valid) versions of the same block. We defend against this by detecting
/// > the case where we would hash two identical hashes at the end of the list
/// > together, and treating that identically to the block having an invalid
/// > merkle root. Assuming no double-SHA256 collisions, this will detect all
/// > known ways of changing the transactions without affecting the merkle
/// > root.
///
/// This vulnerability does not apply to Zebra, because it does not store invalid
/// data on disk, and because it does not permanently fail blocks or use an
/// aggressive anti-DoS mechanism.
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Root(pub [u8; 32]);

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root").field(&hex::encode(&self.0)).finish()
    }
}

fn hash(h1: &[u8; 32], h2: &[u8; 32]) -> [u8; 32] {
    let mut w = sha256d::Writer::default();
    w.write_all(h1).unwrap();
    w.write_all(h2).unwrap();
    w.finish()
}

/// Compute the root of a Merke tree as used in Bitcoin.
/// `hashes` must contain the hashes of the tree leaves.
/// The root is written to the the first element of the input vector.
/// See [`Root`] for an important disclaimer.
fn root(hashes: &mut Vec<[u8; 32]>) {
    while hashes.len() > 1 {
        *hashes = hashes
            .chunks(2)
            .map(|chunk| match chunk {
                [h1, h2] => hash(h1, h2),
                [h1] => hash(h1, h1),
                _ => unreachable!("chunks(2)"),
            })
            .collect();
    }
}

impl<T> std::iter::FromIterator<T> for Root
where
    T: std::convert::AsRef<Transaction>,
{
    fn from_iter<I>(transactions: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        transactions
            .into_iter()
            .map(|tx| tx.as_ref().hash())
            .collect()
    }
}

impl std::iter::FromIterator<transaction::Hash> for Root {
    fn from_iter<I>(hashes: I) -> Self
    where
        I: IntoIterator<Item = transaction::Hash>,
    {
        let mut hashes = hashes.into_iter().map(|hash| hash.0).collect::<Vec<_>>();
        root(&mut hashes);
        Self(hashes[0])
    }
}

/// The root of the authorizing data Merkle tree, binding the
/// block header to the authorizing data of the block (signatures, proofs)
/// as defined in [ZIP-244].
///
/// See [`Root`] for an important disclaimer.
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct AuthDataRoot(pub [u8; 32]);

impl fmt::Debug for AuthDataRoot {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("AuthRoot")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl<T> std::iter::FromIterator<T> for AuthDataRoot
where
    T: std::convert::AsRef<Transaction>,
{
    fn from_iter<I>(transactions: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        // > For transaction versions before v5, a placeholder value consisting
        // > of 32 bytes of 0xFF is used in place of the authorizing data commitment.
        // > This is only used in the tree committed to by hashAuthDataRoot.
        transactions
            .into_iter()
            .map(|tx| {
                tx.as_ref()
                    .auth_digest()
                    .unwrap_or_else(|| transaction::AuthDigest([0xFF; 32]))
            })
            .collect()
    }
}

impl std::iter::FromIterator<transaction::AuthDigest> for AuthDataRoot {
    fn from_iter<I>(hashes: I) -> Self
    where
        I: IntoIterator<Item = transaction::AuthDigest>,
    {
        let mut hashes = hashes.into_iter().map(|hash| hash.0).collect::<Vec<_>>();
        root(&mut hashes);
        Self(hashes[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{block::Block, serialization::ZcashDeserialize};

    #[test]
    fn block_test_vectors() {
        for block_bytes in zebra_test::vectors::BLOCKS.iter() {
            let block = Block::zcash_deserialize(&**block_bytes).unwrap();
            let merkle_root = block.transactions.iter().collect::<Root>();
            assert_eq!(
                merkle_root,
                block.header.merkle_root,
                "block: {:?} {:?} transaction hashes: {:?}",
                block.coinbase_height().unwrap(),
                block.hash(),
                block
                    .transactions
                    .iter()
                    .map(|tx| tx.hash())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn auth_digest() {
        for block_bytes in zebra_test::vectors::BLOCKS.iter() {
            let block = Block::zcash_deserialize(&**block_bytes).unwrap();
            let _merkle_root = block.transactions.iter().collect::<AuthDataRoot>();
            // No test vectors for now, so just check it works
        }
    }
}
