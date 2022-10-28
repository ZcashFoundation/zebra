//! Sprout key types.
//!
//! Unused key types are not implemented, see PR #5476.
//!
//! "The receiving key sk_enc, the incoming viewing key ivk = (apk,
//! sk_enc), and the shielded payment address addr_pk = (a_pk, pk_enc) are
//! derived from a_sk, as described in ['Sprout Key Components'][ps]
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents

use std::fmt;

/// A Sprout _paying key_.
///
/// Derived from a Sprout _spending key.
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct PayingKey(pub [u8; 32]);

impl AsRef<[u8]> for PayingKey {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for PayingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("PayingKey")
            .field(&hex::encode(self.0))
            .finish()
    }
}
