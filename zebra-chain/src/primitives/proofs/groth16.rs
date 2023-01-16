//! Groth16 proofs for Zebra.

use std::{fmt, io};

use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// An encoding of a Groth16 proof, as used in Zcash.
#[derive(Serialize, Deserialize)]
pub struct Groth16Proof(#[serde(with = "BigArray")] pub [u8; 192]);

impl fmt::Debug for Groth16Proof {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Groth16Proof")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

impl Copy for Groth16Proof {}

impl Clone for Groth16Proof {
    fn clone(&self) -> Self {
        let mut bytes = [0; 192];
        bytes[..].copy_from_slice(&self.0[..]);
        Self(bytes)
    }
}

impl Eq for Groth16Proof {}

impl From<[u8; 192]> for Groth16Proof {
    fn from(bytes: [u8; 192]) -> Groth16Proof {
        Self(bytes)
    }
}

impl From<Groth16Proof> for [u8; 192] {
    fn from(rt: Groth16Proof) -> [u8; 192] {
        rt.0
    }
}

impl PartialEq for Groth16Proof {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

impl ZcashSerialize for Groth16Proof {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for Groth16Proof {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; 192];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};

#[cfg(any(test, feature = "proptest-impl"))]
impl Arbitrary for Groth16Proof {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 192))
            .prop_map(|v| {
                let mut bytes = [0; 192];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
