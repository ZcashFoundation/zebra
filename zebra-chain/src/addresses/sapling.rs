//! Sapling Shielded Payment Address types.

use std::{fmt, io};

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, array, prelude::*};

use crate::{
    keys::sapling,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
};

///
pub struct SaplingShieldedAddress {
    diversifier: sapling::Diversifier,
    transmission_key: sapling::TransmissionKey,
}
