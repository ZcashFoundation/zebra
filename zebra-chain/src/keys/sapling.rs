//! Sapling key types
//!
//! "The spend authorizing key ask, proof authorizing key (ak, nsk),
//! full viewing key (ak, nk, ovk), incoming viewing key ivk, and each
//! diversified payment address addr_d = (d, pk_d ) are derived from sk,
//! as described in [Sapling Key Components][ps]."
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents

use std::{
    fmt,
    io::{self},
};

#[cfg(test)]
use proptest::{array, collection::vec, prelude::*};
#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

#[cfg(test)]
proptest! {

    //#[test]
    // fn test() {}
}
