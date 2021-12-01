use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};
use std::io::{self, Read};

/// A sequence of message authentication tags ...
///
/// binding h_sig to each a_sk of the JoinSplit description, computed as
/// described in § 4.10 'Non-malleability (Sprout)' on p. 37
#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Mac([u8; 32]);

impl From<[u8; 32]> for Mac {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<Mac> for [u8; 32] {
    fn from(rt: Mac) -> [u8; 32] {
        rt.0
    }
}

impl From<&Mac> for [u8; 32] {
    fn from(mac: &Mac) -> Self {
        mac.clone().into()
    }
}

impl ZcashDeserialize for Mac {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let bytes = reader.read_32_bytes()?;

        Ok(Self(bytes))
    }
}

impl ZcashSerialize for Mac {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])
    }
}
