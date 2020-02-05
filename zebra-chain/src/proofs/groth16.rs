use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, io};

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// An encoding of a Groth16 proof, as used in Zcash.
pub struct Groth16Proof(pub [u8; 192]);

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

impl PartialEq for Groth16Proof {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

impl Eq for Groth16Proof {}

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

impl<'de> Deserialize<'de> for Groth16Proof {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Groth16Visitor;
        impl<'de> serde::de::Visitor<'de> for Groth16Visitor {
            type Value = Groth16Proof;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("192 bytes of data")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Groth16Proof, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut bytes = [0u8; 192];
                for i in 0..192 {
                    bytes[i] = seq
                        .next_element()?
                        .ok_or(serde::de::Error::invalid_length(i, &"expected 192 bytes"))?;
                }
                Ok(Groth16Proof(bytes))
            }
        }

        deserializer.deserialize_tuple(192, Groth16Visitor)
    }
}

impl Serialize for Groth16Proof {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeTuple;
        let mut tup = serializer.serialize_tuple(192)?;
        for byte in &self.0[..] {
            tup.serialize_element(byte)?;
        }
        tup.end()
    }
}

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};

#[cfg(test)]
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
