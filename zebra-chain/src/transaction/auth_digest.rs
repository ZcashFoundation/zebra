use crate::primitives::zcash_primitives::auth_digest;

use super::Transaction;

/// An authorizing data commitment hash as specified in [ZIP-244].
///
/// [ZIP-244]: https://zips.z.cash/zip-0244..
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct AuthDigest(pub(crate) [u8; 32]);

impl<'a> From<&'a Transaction> for AuthDigest {
    /// Computes the authorizing data commitment for a transaction.
    ///
    /// # Panics
    ///
    /// If passed a pre-v5 transaction.
    fn from(transaction: &'a Transaction) -> Self {
        auth_digest(transaction)
    }
}
