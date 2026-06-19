use proptest::prelude::*;
use std::io::Cursor;

use crate::{
    orchard,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};

use crate::orchard::shielded_data::FlagFormat;

proptest! {
    /// Make sure only valid flags deserialize.
    ///
    /// The default `Flags` codec is the pre-NU6.3 format (v5 Orchard bundles), where bits 2..7 are
    /// reserved and MUST be zero. We use [`Flags::from_byte`] in the pre-NU6.3 format as the oracle
    /// for what the default codec accepts. (The NU6.3 format, which permits bit 2, is covered by
    /// the `flags` unit tests.)
    #[test]
    fn flag_roundtrip_bytes(flags in any::<u8>()) {

        let mut serialized = Cursor::new(Vec::new());
        flags.zcash_serialize(&mut serialized)?;

        serialized.set_position(0);
        let maybe_deserialized = (&mut serialized).zcash_deserialize_into();

        match orchard::Flags::from_byte(flags, FlagFormat::PreNu6_3) {
            Ok(valid_flags) => {
                prop_assert_eq!(maybe_deserialized.ok(), Some(valid_flags));
            }
            Err(_) => {
                prop_assert_eq!(
                    maybe_deserialized.err().unwrap().to_string(),
                    "parse error: invalid reserved orchard flags"
                );
            }
        }
    }
}
