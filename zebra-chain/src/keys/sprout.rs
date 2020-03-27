//! Sprout key types
//!
//! "The receiving key sk_enc, the incoming viewing key ivk = (apk,
//! sk_enc), and the shielded payment address addr_pk = (a_pk, pk_enc) are
//! derived from a_sk, as described in [‘Sprout Key Components’][ps]
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents

use std::fmt;

use byteorder::{ByteOrder, LittleEndian};

#[cfg(test)]
use proptest::{array, collection::vec, prelude::*};
#[cfg(test)]
use proptest_derive::Arbitrary;

use sha2;

/// Our root secret key of the Sprout key derivation tree.
///
/// All other Sprout key types derive from the SpendingKey value.
/// Actually 252 bits.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpendingKey(pub [u8; 32]);

/// Derived from a _SpendingKey_.
pub type ReceivingKey = x25519_dalek::StaticSecret;

impl From<SpendingKey> for ReceivingKey {
    /// For this invocation of SHA256Compress as the PRF, t=0, which
    /// is populated by default in an empty block of all zeros to
    /// start.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> ReceivingKey {
        let mut state = [0u32; 8];
        let mut block = [0u8; 64]; // Thus, t = 0

        block[0..32].copy_from_slice(&spending_key.0[..]);
        block[0] |= 0b1100_0000;

        sha2::compress256(&mut state, &block);

        let mut derived_bytes = [0u8; 32];
        LittleEndian::write_u32_into(&state, &mut derived_bytes);

        ReceivingKey::from(derived_bytes)
    }
}

/// Derived from a _SpendingKey_.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct PayingKey(pub [u8; 32]);

impl fmt::Debug for PayingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("PayingKey")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl From<SpendingKey> for PayingKey {
    /// For this invocation of SHA256Compress as the PRF, t=1.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> PayingKey {
        let mut state = [0u32; 8];
        let mut block = [0u8; 64];

        block[0..32].copy_from_slice(&spending_key.0[..]);
        block[0] |= 0b1100_0000;

        block[32] = 1u8; // t = 1

        sha2::compress256(&mut state, &block);

        let mut derived_bytes = [0u8; 32];
        LittleEndian::write_u32_into(&state, &mut derived_bytes);

        PayingKey(derived_bytes)
    }
}

/// Derived from a _ReceivingKey_.
pub type TransmissionKey = x25519_dalek::PublicKey;

///
pub struct IncomingViewingKey {
    paying_key: PayingKey,
    receiving_key: ReceivingKey,
}

#[cfg(test)]
mod tests {

    use rand_core::{OsRng, RngCore};

    use super::*;

    #[test]
    // TODO: test vectors, not just random data
    fn derive_keys() {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);

        let spending_key = SpendingKey(bytes);

        println!("{:?}", spending_key);

        let receiving_key = ReceivingKey::from(spending_key);

        // println!("{}", receiving_key);

        let transmission_key = TransmissionKey::from(&receiving_key);

        println!("{:?}", transmission_key);
    }
}

#[cfg(test)]
proptest! {

    // #[test]
    // fn test() {}
}
