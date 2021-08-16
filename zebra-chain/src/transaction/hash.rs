//! Transaction identifiers for Zcash.
//!
//! Zcash has two different transaction identifiers, with different widths:
//! * [`Hash`]: a 32-byte narrow transaction ID, which uniquely identifies mined transactions
//!   (transactions that have been committed to the blockchain in blocks), and
//! * [`WtxId`]: a 64-byte wide transaction ID, which uniquely identifies unmined transactions
//!   (transactions that are sent by wallets or stored in node mempools).
//!
//! Transaction versions 1-4 are uniquely identified by narrow transaction IDs,
//! so Zebra and the Zcash network protocol don't use wide transaction IDs for them.
//!
//! Zebra's [`UnminedTxId`] and [`UnminedTx`] enums provide the correct ID for the
//! transaction version. They can be used to handle transactions regardless of version,
//! and get the [`WtxId`] or [`Hash`] when required.

use std::{
    convert::{TryFrom, TryInto},
    fmt,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use serde::{Deserialize, Serialize};

use crate::serialization::{
    ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
};

use super::{txid::TxIdBuilder, AuthDigest, Transaction};

/// A narrow transaction ID, which uniquely identifies mined v5 transactions,
/// and all v1-v4 transactions.
///
/// Note: Zebra displays transaction and block hashes in big-endian byte-order,
/// following the u256 convention set by Bitcoin and zcashd.
///
/// "The transaction ID of a version 4 or earlier transaction is the SHA-256d hash
/// of the transaction encoding in the pre-v5 format described above.
///
/// The transaction ID of a version 5 transaction is as defined in [ZIP-244]."
/// [Spec: Transaction Identifiers]
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
/// [Spec: Transaction Identifiers]: https://zips.z.cash/protocol/protocol.pdf#txnidentifiers
#[derive(Copy, Clone, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Hash(pub [u8; 32]);

impl From<Transaction> for Hash {
    fn from(transaction: Transaction) -> Self {
        // use the ref implementation, to avoid cloning the transaction
        Hash::from(&transaction)
    }
}

impl From<&Transaction> for Hash {
    fn from(transaction: &Transaction) -> Self {
        let hasher = TxIdBuilder::new(transaction);
        hasher
            .txid()
            .expect("zcash_primitives and Zebra transaction formats must be compatible")
    }
}

impl From<[u8; 32]> for Hash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<Hash> for [u8; 32] {
    fn from(hash: Hash) -> Self {
        hash.0
    }
}

impl From<&Hash> for [u8; 32] {
    fn from(hash: &Hash) -> Self {
        (*hash).into()
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut reversed_bytes = self.0;
        reversed_bytes.reverse();
        f.write_str(&hex::encode(&reversed_bytes))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut reversed_bytes = self.0;
        reversed_bytes.reverse();
        f.debug_tuple("transaction::Hash")
            .field(&hex::encode(reversed_bytes))
            .finish()
    }
}

impl std::str::FromStr for Hash {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0; 32];
        if hex::decode_to_slice(s, &mut bytes[..]).is_err() {
            Err(SerializationError::Parse("hex decoding error"))
        } else {
            bytes.reverse();
            Ok(Hash(bytes))
        }
    }
}

impl ZcashSerialize for Hash {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_32_bytes(&self.into())
    }
}

impl ZcashDeserialize for Hash {
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(reader.read_32_bytes()?.into())
    }
}

/// A wide transaction ID, which uniquely identifies unmined v5 transactions.
///
/// Wide transaction IDs are not used for transaction versions 1-4.
///
/// "A v5 transaction also has a wtxid (used for example in the peer-to-peer protocol)
/// as defined in [ZIP-239]."
/// [Spec: Transaction Identifiers]
///
/// [ZIP-239]: https://zips.z.cash/zip-0239
/// [Spec: Transaction Identifiers]: https://zips.z.cash/protocol/protocol.pdf#txnidentifiers
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct WtxId {
    /// The non-malleable transaction ID for this transaction's effects.
    pub id: Hash,

    /// The authorizing data digest for this transactions signatures, proofs, and scripts.
    pub auth_digest: AuthDigest,
}

impl WtxId {
    /// Return this wide transaction ID as a serialized byte array.
    pub fn as_bytes(&self) -> [u8; 64] {
        <[u8; 64]>::from(self)
    }
}

impl From<Transaction> for WtxId {
    /// Computes the wide transaction ID for a transaction.
    ///
    /// # Panics
    ///
    /// If passed a pre-v5 transaction.
    fn from(transaction: Transaction) -> Self {
        // use the ref implementation, to avoid cloning the transaction
        WtxId::from(&transaction)
    }
}

impl From<&Transaction> for WtxId {
    /// Computes the wide transaction ID for a transaction.
    ///
    /// # Panics
    ///
    /// If passed a pre-v5 transaction.
    fn from(transaction: &Transaction) -> Self {
        Self {
            id: transaction.into(),
            auth_digest: transaction.into(),
        }
    }
}

impl From<[u8; 64]> for WtxId {
    fn from(bytes: [u8; 64]) -> Self {
        let id: [u8; 32] = bytes[0..32].try_into().expect("length is 64");
        let auth_digest: [u8; 32] = bytes[32..64].try_into().expect("length is 64");

        Self {
            id: id.into(),
            auth_digest: auth_digest.into(),
        }
    }
}

impl From<WtxId> for [u8; 64] {
    fn from(wtx_id: WtxId) -> Self {
        let mut bytes = [0; 64];
        let (id, auth_digest) = bytes.split_at_mut(32);

        id.copy_from_slice(&wtx_id.id.0);
        auth_digest.copy_from_slice(&wtx_id.auth_digest.0);

        bytes
    }
}

impl From<&WtxId> for [u8; 64] {
    fn from(wtx_id: &WtxId) -> Self {
        (*wtx_id).into()
    }
}

impl TryFrom<&[u8]> for WtxId {
    type Error = SerializationError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let bytes: [u8; 64] = bytes.try_into()?;

        Ok(bytes.into())
    }
}

impl TryFrom<Vec<u8>> for WtxId {
    type Error = SerializationError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        bytes.as_slice().try_into()
    }
}

impl TryFrom<&Vec<u8>> for WtxId {
    type Error = SerializationError;

    fn try_from(bytes: &Vec<u8>) -> Result<Self, Self::Error> {
        bytes.clone().try_into()
    }
}

impl fmt::Display for WtxId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.id.to_string())?;
        f.write_str(&self.auth_digest.to_string())
    }
}

impl std::str::FromStr for WtxId {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // we need to split using bytes,
        // because str::split_at panics if it splits a UTF-8 codepoint
        let s = s.as_bytes();

        if s.len() == 128 {
            let (id, auth_digest) = s.split_at(64);
            let id = std::str::from_utf8(id)?;
            let auth_digest = std::str::from_utf8(auth_digest)?;

            Ok(Self {
                id: id.parse()?,
                auth_digest: auth_digest.parse()?,
            })
        } else {
            Err(SerializationError::Parse(
                "wrong length for WtxId hex string",
            ))
        }
    }
}

impl ZcashSerialize for WtxId {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_64_bytes(&self.into())
    }
}

impl ZcashDeserialize for WtxId {
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(reader.read_64_bytes()?.into())
    }
}
