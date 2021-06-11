use proptest::prelude::*;
use std::io::Cursor;

use super::super::super::*;

use crate::serialization::{ZcashDeserializeInto, ZcashSerialize};

proptest! {
    #[test]
    // Make sure only valid flags deserialize
    fn flags_roundtrip(flags in any::<u8>()) {

        let mut serialized = Cursor::new(Vec::new());
        flags.zcash_serialize(&mut serialized)?;

        serialized.set_position(0);
        let maybe_deserialized = (&mut serialized).zcash_deserialize_into();

        let mut error_catch = String::new();
        let maybe_deserialized: Option<orchard::Flags> = match maybe_deserialized {
            Ok(deserialized) => Some(deserialized),
            Err(e) => {
                error_catch = e.to_string();
                None
            },
        };

        let binary_repr = format!("{:b}", flags);
        if maybe_deserialized.is_some() {
            let flag = maybe_deserialized.unwrap();
            prop_assert!(binary_repr.len() <= 2);
            prop_assert!(
                flag == orchard::Flags::empty()
                || flag == orchard::Flags::ENABLE_SPENDS
                || flag == orchard::Flags::ENABLE_OUTPUTS
                || flag == orchard::Flags::ENABLE_SPENDS | orchard::Flags::ENABLE_OUTPUTS);
        }
        else {
            prop_assert!(binary_repr.len() > 2);
            prop_assert_eq!(error_catch, "parse error: invalid orchard flags");
        }
    }
}
