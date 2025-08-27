//! The Bitcoin-inherited Merkle tree of transactions.

use std::{fmt, io::Write};

use hex::{FromHex, ToHex};

use crate::{
    serialization::{sha256d, BytesInDisplayOrder},
    transaction::{self, Transaction, UnminedTx, UnminedTxId, VerifiedUnminedTx},
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

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
/// > for transaction lists \[1,2,3,4,5,6\] and \[1,2,3,4,5,6,5,6\] (where 5 and
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
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary, Default))]
pub struct Root(pub [u8; 32]);

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root").field(&hex::encode(self.0)).finish()
    }
}

impl From<[u8; 32]> for Root {
    fn from(hash: [u8; 32]) -> Self {
        Root(hash)
    }
}

impl From<Root> for [u8; 32] {
    fn from(hash: Root) -> Self {
        hash.0
    }
}

impl BytesInDisplayOrder<true, 32> for Root {
    fn bytes_in_serialized_order(&self) -> [u8; 32] {
        self.0
    }

    fn from_bytes_in_serialized_order(bytes: [u8; 32]) -> Self {
        Root(bytes)
    }
}

impl ToHex for &Root {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for Root {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for Root {
    type Error = <[u8; 32] as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let mut hash = <[u8; 32]>::from_hex(hex)?;
        hash.reverse();

        Ok(hash.into())
    }
}

fn hash(h1: &[u8; 32], h2: &[u8; 32]) -> [u8; 32] {
    let mut w = sha256d::Writer::default();
    w.write_all(h1).unwrap();
    w.write_all(h2).unwrap();
    w.finish()
}

fn auth_data_hash(h1: &[u8; 32], h2: &[u8; 32]) -> [u8; 32] {
    // > Non-leaf hashes in this tree are BLAKE2b-256 hashes personalized by
    // > the string "ZcashAuthDatHash".
    // https://zips.z.cash/zip-0244#block-header-changes
    blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZcashAuthDatHash")
        .to_state()
        .update(h1)
        .update(h2)
        .finalize()
        .as_bytes()
        .try_into()
        .expect("32 byte array")
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

impl std::iter::FromIterator<UnminedTx> for Root {
    fn from_iter<I>(transactions: I) -> Self
    where
        I: IntoIterator<Item = UnminedTx>,
    {
        transactions
            .into_iter()
            .map(|tx| tx.id.mined_id())
            .collect()
    }
}

impl std::iter::FromIterator<UnminedTxId> for Root {
    fn from_iter<I>(tx_ids: I) -> Self
    where
        I: IntoIterator<Item = UnminedTxId>,
    {
        tx_ids.into_iter().map(|tx_id| tx_id.mined_id()).collect()
    }
}

impl std::iter::FromIterator<VerifiedUnminedTx> for Root {
    fn from_iter<I>(transactions: I) -> Self
    where
        I: IntoIterator<Item = VerifiedUnminedTx>,
    {
        transactions
            .into_iter()
            .map(|tx| tx.transaction.id.mined_id())
            .collect()
    }
}

impl std::iter::FromIterator<transaction::Hash> for Root {
    /// # Panics
    ///
    /// When there are no transactions in the iterator.
    /// This is impossible, because every block must have a coinbase transaction.
    fn from_iter<I>(hashes: I) -> Self
    where
        I: IntoIterator<Item = transaction::Hash>,
    {
        let mut hashes = hashes.into_iter().map(|hash| hash.0).collect::<Vec<_>>();
        while hashes.len() > 1 {
            hashes = hashes
                .chunks(2)
                .map(|chunk| match chunk {
                    [h1, h2] => hash(h1, h2),
                    [h1] => hash(h1, h1),
                    _ => unreachable!("chunks(2)"),
                })
                .collect();
        }
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
pub struct AuthDataRoot(pub(crate) [u8; 32]);

impl fmt::Debug for AuthDataRoot {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("AuthRoot")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl From<[u8; 32]> for AuthDataRoot {
    fn from(hash: [u8; 32]) -> Self {
        AuthDataRoot(hash)
    }
}

impl From<AuthDataRoot> for [u8; 32] {
    fn from(hash: AuthDataRoot) -> Self {
        hash.0
    }
}

impl BytesInDisplayOrder<true, 32> for AuthDataRoot {
    fn bytes_in_serialized_order(&self) -> [u8; 32] {
        self.0
    }

    fn from_bytes_in_serialized_order(bytes: [u8; 32]) -> Self {
        AuthDataRoot(bytes)
    }
}

impl ToHex for &AuthDataRoot {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for AuthDataRoot {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for AuthDataRoot {
    type Error = <[u8; 32] as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let mut hash = <[u8; 32]>::from_hex(hex)?;
        hash.reverse();

        Ok(hash.into())
    }
}

/// The placeholder used for the [`AuthDigest`](transaction::AuthDigest) of pre-v5 transactions.
///
/// # Consensus
///
/// > For transaction versions before v5, a placeholder value consisting
/// > of 32 bytes of 0xFF is used in place of the authorizing data commitment.
/// > This is only used in the tree committed to by hashAuthDataRoot.
///
/// <https://zips.z.cash/zip-0244#authorizing-data-commitment>
pub const AUTH_DIGEST_PLACEHOLDER: transaction::AuthDigest = transaction::AuthDigest([0xFF; 32]);

impl<T> std::iter::FromIterator<T> for AuthDataRoot
where
    T: std::convert::AsRef<Transaction>,
{
    fn from_iter<I>(transactions: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        transactions
            .into_iter()
            .map(|tx| tx.as_ref().auth_digest().unwrap_or(AUTH_DIGEST_PLACEHOLDER))
            .collect()
    }
}

impl std::iter::FromIterator<UnminedTx> for AuthDataRoot {
    fn from_iter<I>(transactions: I) -> Self
    where
        I: IntoIterator<Item = UnminedTx>,
    {
        transactions
            .into_iter()
            .map(|tx| tx.id.auth_digest().unwrap_or(AUTH_DIGEST_PLACEHOLDER))
            .collect()
    }
}

impl std::iter::FromIterator<VerifiedUnminedTx> for AuthDataRoot {
    fn from_iter<I>(transactions: I) -> Self
    where
        I: IntoIterator<Item = VerifiedUnminedTx>,
    {
        transactions
            .into_iter()
            .map(|tx| {
                tx.transaction
                    .id
                    .auth_digest()
                    .unwrap_or(AUTH_DIGEST_PLACEHOLDER)
            })
            .collect()
    }
}

impl std::iter::FromIterator<UnminedTxId> for AuthDataRoot {
    fn from_iter<I>(tx_ids: I) -> Self
    where
        I: IntoIterator<Item = UnminedTxId>,
    {
        tx_ids
            .into_iter()
            .map(|tx_id| tx_id.auth_digest().unwrap_or(AUTH_DIGEST_PLACEHOLDER))
            .collect()
    }
}

impl std::iter::FromIterator<transaction::AuthDigest> for AuthDataRoot {
    fn from_iter<I>(hashes: I) -> Self
    where
        I: IntoIterator<Item = transaction::AuthDigest>,
    {
        let mut hashes = hashes.into_iter().map(|hash| hash.0).collect::<Vec<_>>();
        // > This new commitment is named hashAuthDataRoot and is the root of a
        // > binary Merkle tree of transaction authorizing data commitments [...]
        // > padded with leaves having the "null" hash value [0u8; 32].
        // https://zips.z.cash/zip-0244#block-header-changes
        // Pad with enough leaves to make the tree full (a power of 2).
        let pad_count = hashes.len().next_power_of_two() - hashes.len();
        hashes.extend(std::iter::repeat_n([0u8; 32], pad_count));
        assert!(hashes.len().is_power_of_two());

        while hashes.len() > 1 {
            hashes = hashes
                .chunks(2)
                .map(|chunk| match chunk {
                    [h1, h2] => auth_data_hash(h1, h2),
                    _ => unreachable!("number of nodes is always even since tree is full"),
                })
                .collect();
        }

        Self(hashes[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{block::Block, serialization::ZcashDeserialize, transaction::AuthDigest};

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
            let _auth_root = block.transactions.iter().collect::<AuthDataRoot>();
            // No test vectors for now, so just check it computes without panicking
        }
    }

    #[test]
    fn auth_data_padding() {
        // Compute the root of a 3-leaf tree with arbitrary leaves
        let mut v = vec![
            AuthDigest([0x42; 32]),
            AuthDigest([0xAA; 32]),
            AuthDigest([0x77; 32]),
        ];
        let root_3 = v.iter().copied().collect::<AuthDataRoot>();

        // Compute the root a 4-leaf tree with the same leaves as before and
        // an additional all-zeroes leaf.
        // Since this is the same leaf used as padding in the previous tree,
        // then both trees must have the same root.
        v.push(AuthDigest([0x00; 32]));
        let root_4 = v.iter().copied().collect::<AuthDataRoot>();

        assert_eq!(root_3, root_4);
    }

    #[test]
    fn auth_data_pre_v5() {
        // Compute the AuthDataRoot for a single transaction of an arbitrary pre-V5 block
        let block =
            Block::zcash_deserialize(&**zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES).unwrap();
        let auth_root = block.transactions.iter().take(1).collect::<AuthDataRoot>();

        // Compute the AuthDataRoot with a single [0xFF; 32] digest.
        // Since ZIP-244 specifies that this value must be used as the auth digest of
        // pre-V5 transactions, then the roots must match.
        let expect_auth_root = [AuthDigest([0xFF; 32])]
            .iter()
            .copied()
            .collect::<AuthDataRoot>();

        assert_eq!(auth_root, expect_auth_root);
    }
}
