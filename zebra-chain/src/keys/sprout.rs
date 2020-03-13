//! Sprout key types
//!
//! "The receiving key sk_enc, the incoming viewing key ivk = (apk,
//! sk_enc), and the shielded payment address addr_pk = (a_pk, pk_enc) are
//! derived from a_sk, as described in [‘Sprout Key Components’][ps]
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents

use std::{
    fmt,
    io::{self},
};

#[cfg(test)]
use proptest::{array, collection::vec, prelude::*};
#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// All other Sprout key types derive from the SpendingKey value.
pub struct SpendingKey;

pub struct ReceivingKey;

pub struct PayingKey;

pub struct TransmissionKey;

pub struct IncomingViewingKey {
    paying_key: PayingKey,
    receiving_key: ReceivingKey,
}

#[cfg(test)]
proptest! {

    // #[test]
    // fn test() {}
}
