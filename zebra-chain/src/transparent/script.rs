//! Bitcoin script for Zebra

use std::{fmt, io};

use crate::serialization::{
    zcash_serialize_bytes, SerializationError, ZcashDeserialize, ZcashSerialize,
};

/// An encoding of a Bitcoin script.
#[derive(Clone, Eq, PartialEq, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary, Serialize, Deserialize)
)]
pub struct Script(
    /// # Correctness
    ///
    /// Consensus-critical serialization uses [`ZcashSerialize`].
    /// [`serde`]-based hex serialization must only be used for testing.
    #[cfg_attr(any(test, feature = "proptest-impl"), serde(with = "hex"))]
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

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Script")
            .field(&hex::encode(&self.0))
            .finish()
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
    use crate::serialization::{ZcashDeserialize, ZcashSerialize};

    proptest! {
        #[test]
        fn script_roundtrip(script in any::<Script>()) {
            zebra_test::init();

            let mut bytes = Cursor::new(Vec::new());
            script.zcash_serialize(&mut bytes)?;

            bytes.set_position(0);
            let other_script = Script::zcash_deserialize(&mut bytes)?;

            prop_assert_eq![script, other_script];
        }
    }
}
