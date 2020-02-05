use std::{fmt, io};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// An encoding of a BCTV14 proof, as used in Zcash.
pub struct Bctv14Proof(pub [u8; 296]);

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
        let mut bytes = [0; 296];
        bytes[..].copy_from_slice(&self.0[..]);
        Self(bytes)
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

impl<'de> Deserialize<'de> for Bctv14Proof {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Bctv14Visitor;
        impl<'de> serde::de::Visitor<'de> for Bctv14Visitor {
            type Value = Bctv14Proof;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("296 bytes of data")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Bctv14Proof, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut bytes = [0u8; 296];
                for i in 0..296 {
                    bytes[i] = seq
                        .next_element()?
                        .ok_or(serde::de::Error::invalid_length(i, &"expected 296 bytes"))?;
                }
                Ok(Bctv14Proof(bytes))
            }
        }

        deserializer.deserialize_tuple(296, Bctv14Visitor)
    }
}

impl Serialize for Bctv14Proof {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeTuple;
        let mut tup = serializer.serialize_tuple(296)?;
        for byte in &self.0[..] {
            tup.serialize_element(byte)?;
        }
        tup.end()
    }
}

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};

#[cfg(test)]
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
