use serde::{Deserialize, Serialize};
use std::{fmt, io};

use crate::serialization::{serde_helpers, SerializationError, ZcashDeserialize, ZcashSerialize};

/// An encoding of a Halo2 proof, as used in [Zcash][halo2].
///
/// Halo2 proofs in Zcash Orchard do not have a fixed size, hence the newtype
/// around a vector of bytes.
///
/// [halo2]: https://zips.z.cash/protocol/nu5.pdf#halo2
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Halo2Proof(pub Vec<u8>);

impl fmt::Debug for Halo2Proof {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Halo2Proof")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

// impl Copy for Halo2Proof {}

// impl Clone for Halo2Proof {
//     fn clone(&self) -> Self {
//         let mut bytes = [0; 192];
//         bytes[..].copy_from_slice(&self.0[..]);
//         Self(bytes)
//     }
// }

// impl PartialEq for Halo2Proof {
//     fn eq(&self, other: &Self) -> bool {
//         self.0[..] == other.0[..]
//     }
// }

// impl Eq for Halo2Proof {}

impl ZcashSerialize for Halo2Proof {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for Halo2Proof {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;

        Ok(Self(bytes))
    }
}
// TODO: figure how a Halo2Proof Strategy for generating proofs for proptesting.
// #[cfg(any(test, feature = "proptest-impl"))]
// use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};
//
// #[cfg(any(test, feature = "proptest-impl"))]
// impl Arbitrary for Halo2Proof {
//     type Parameters = ();

//     fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
//         (vec(any::<u8>(), 192))
//             .prop_map(|v| {
//                 let mut bytes = [0; 192];
//                 bytes.copy_from_slice(v.as_slice());
//                 Self(bytes)
//             })
//             .boxed()
//     }

//     type Strategy = BoxedStrategy<Self>;
// }
