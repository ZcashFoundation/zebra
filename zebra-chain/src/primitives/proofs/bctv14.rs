//! BCTV14 proofs for Zebra.

use std::{fmt, io};

use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// An encoding of a BCTV14 proof, as used in Zcash.
#[derive(Serialize, Deserialize)]
pub struct Bctv14Proof(#[serde(with = "BigArray")] pub [u8; 296]);

impl fmt::Debug for Bctv14Proof {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Bctv14Proof")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

impl Copy for Bctv14Proof {}

impl Clone for Bctv14Proof {
    fn clone(&self) -> Self {
        *self
    }
}

impl PartialEq for Bctv14Proof {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

impl Eq for Bctv14Proof {}

impl ZcashSerialize for Bctv14Proof {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for Bctv14Proof {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; 296];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};

#[cfg(any(test, feature = "proptest-impl"))]
impl Arbitrary for Bctv14Proof {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 296))
            .prop_map(|v| {
                let mut bytes = [0; 296];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
