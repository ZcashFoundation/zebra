//! Bitcoin script for Zebra

use std::{fmt, io};

use hex::{FromHex, FromHexError, ToHex};

use crate::serialization::{
    zcash_serialize_bytes, SerializationError, ZcashDeserialize, ZcashSerialize,
};

/// An encoding of a Bitcoin script.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Script(
    /// # Correctness
    ///
    /// Consensus-critical serialization uses [`ZcashSerialize`].
    /// [`serde`]-based hex serialization must only be used for RPCs and testing.
    #[serde(with = "hex")]
    Vec<u8>,
);

impl Script {
    /// Create a new Bitcoin script from its raw bytes.
    /// The raw bytes must not contain the length prefix.
    pub fn new(raw_bytes: &[u8]) -> Self {
        Script(raw_bytes.to_vec())
    }

    /// Return the raw bytes of the script without the length prefix.
    ///
    /// # Correctness
    ///
    /// These raw bytes do not have a length prefix.
    /// The Zcash serialization format requires a length prefix; use `zcash_serialize`
    /// and `zcash_deserialize` to create byte data with a length prefix.
    pub fn as_raw_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<zcash_transparent::address::Script> for Script {
    fn from(script: zcash_transparent::address::Script) -> Self {
        Script(script.0)
    }
}

impl fmt::Display for Script {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Script")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl ToHex for &Script {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.as_raw_bytes().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.as_raw_bytes().encode_hex_upper()
    }
}

impl ToHex for Script {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for Script {
    type Error = FromHexError;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let bytes = Vec::from_hex(hex)?;
        Ok(Script::new(&bytes))
    }
}

impl ZcashSerialize for Script {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        zcash_serialize_bytes(&self.0, writer)
    }
}

impl ZcashDeserialize for Script {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        Vec::zcash_deserialize(reader).map(Script)
    }
}

#[cfg(test)]
mod proptests {
    use std::io::Cursor;

    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn script_roundtrip(script in any::<Script>()) {
            let _init_guard = zebra_test::init();

            let mut bytes = Cursor::new(Vec::new());
            script.zcash_serialize(&mut bytes)?;

            bytes.set_position(0);
            let other_script = Script::zcash_deserialize(&mut bytes)?;

            prop_assert_eq![script, other_script];
        }
    }
}
