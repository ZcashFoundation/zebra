use proptest::prelude::*;
use std::io::Cursor;

use crate::{
    orchard,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};

proptest! {
    /// Make sure only valid flags deserialize
    #[test]
    fn flag_roundtrip_bytes(flags in any::<u8>()) {

        let mut serialized = Cursor::new(Vec::new());
        flags.zcash_serialize(&mut serialized)?;

        serialized.set_position(0);
        let maybe_deserialized = (&mut serialized).zcash_deserialize_into();

        let invalid_bits_mask = !orchard::Flags::all().bits();
        match orchard::Flags::from_bits(flags) {
            Some(valid_flags) => {
                prop_assert_eq!(maybe_deserialized.ok(), Some(valid_flags));
                prop_assert_eq!(flags & invalid_bits_mask, 0);
            }
            None => {
                prop_assert_eq!(
                    maybe_deserialized.err().unwrap().to_string(),
                    "parse error: invalid reserved orchard flags"
                );
                prop_assert_ne!(flags & invalid_bits_mask, 0);
            }
        }
    }
}
