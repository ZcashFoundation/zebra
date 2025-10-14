use serde::{Deserialize, Serialize};
use std::{fmt, io};

use crate::serialization::{
    zcash_serialize_bytes, SerializationError, ZcashDeserialize, ZcashSerialize,
};

/// An encoding of a Halo2 proof, as used in [Zcash][halo2].
///
/// Halo2 proofs in Zcash Orchard do not have a fixed size, hence the newtype
/// around a vector of bytes.
///
/// [halo2]: https://zips.z.cash/protocol/nu5.pdf#halo2
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Halo2Proof(pub Vec<u8>);

impl Halo2Proof {
    /// Encode as bytes for RPC usage.
    pub fn bytes_in_display_order(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl fmt::Debug for Halo2Proof {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Halo2Proof")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

impl ZcashSerialize for Halo2Proof {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        zcash_serialize_bytes(&self.0, writer)
    }
}

impl ZcashDeserialize for Halo2Proof {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let proof = Vec::zcash_deserialize(&mut reader)?;

        Ok(Self(proof))
    }
}
#[cfg(any(test, feature = "proptest-impl"))]
use proptest::prelude::*;

#[cfg(any(test, feature = "proptest-impl"))]
impl Arbitrary for Halo2Proof {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (any::<Vec<u8>>()).prop_map(Self).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
