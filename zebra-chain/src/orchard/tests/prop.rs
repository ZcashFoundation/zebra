use proptest::prelude::*;
use std::io::Cursor;

use crate::{
    orchard,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};

proptest! {
    /// Make sure only valid flags deserialize.
    ///
    /// The default `Flags` codec is the pre-NU6.3 format (v5 Orchard bundles), where bits 2..7 are
    /// reserved and MUST be zero. (The NU6.3 format, which permits bit 2 via the `FlagsV6` newtype,
    /// is covered by the `flags` unit tests.)
    #[test]
    fn flag_roundtrip_bytes(flags in any::<u8>()) {

        let mut serialized = Cursor::new(Vec::new());
        flags.zcash_serialize(&mut serialized)?;

        serialized.set_position(0);
        let maybe_deserialized = (&mut serialized).zcash_deserialize_into();

        // The pre-NU6.3 codec accepts the byte iff only the two defined flag bits (0..1) are set.
        let pre_nu6_3_reserved =
            !(orchard::Flags::ENABLE_SPENDS.bits() | orchard::Flags::ENABLE_OUTPUTS.bits());
        if flags & pre_nu6_3_reserved == 0 {
            prop_assert_eq!(
                maybe_deserialized.ok(),
                Some(orchard::Flags::from_bits_truncate(flags))
            );
        } else {
            prop_assert_eq!(
                maybe_deserialized.err().unwrap().to_string(),
                "parse error: invalid reserved orchard flags"
            );
        }
    }
}
