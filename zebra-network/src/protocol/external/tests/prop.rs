use bytes::BytesMut;
use proptest::{collection::vec, prelude::*};
use tokio_util::codec::{Decoder, Encoder};

use zebra_chain::serialization::{
    SerializationError, ZcashDeserializeInto, ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN,
};

use super::super::{Codec, InventoryHash, Message};

/// Maximum number of random input bytes to try to deserialize an [`InventoryHash`] from.
///
/// This is two bytes larger than the maximum [`InventoryHash`] size.
const MAX_INVENTORY_HASH_BYTES: usize = 70;

proptest! {
    /// Test if [`InventoryHash`] is not changed after serializing and deserializing it.
    #[test]
    fn inventory_hash_roundtrip(inventory_hash in any::<InventoryHash>()) {
        let mut bytes = Vec::new();

        let serialization_result = inventory_hash.zcash_serialize(&mut bytes);

        prop_assert!(serialization_result.is_ok());
        prop_assert!(bytes.len() < MAX_INVENTORY_HASH_BYTES);

        let deserialized: Result<InventoryHash, _> = bytes.zcash_deserialize_into();

        prop_assert!(deserialized.is_ok());
        prop_assert_eq!(deserialized.unwrap(), inventory_hash);
    }

    /// Test attempting to deserialize an [`InventoryHash`] from random bytes.
    #[test]
    fn inventory_hash_from_random_bytes(input in vec(any::<u8>(), 0..MAX_INVENTORY_HASH_BYTES)) {
        let deserialized: Result<InventoryHash, _> = input.zcash_deserialize_into();

        if input.len() < 36 {
            // Not enough bytes for any inventory hash
            prop_assert!(deserialized.is_err());
            prop_assert_eq!(
                deserialized.unwrap_err().to_string(),
                "io error: failed to fill whole buffer"
            );
        } else if input[1..4] != [0u8; 3] || input[0] > 5 || input[0] == 4 {
            // Invalid inventory code
            prop_assert!(matches!(
                deserialized,
                Err(SerializationError::Parse("invalid inventory code"))
            ));
        } else if input[0] == 5 && input.len() < 68 {
            // Not enough bytes for a WTX inventory hash
            prop_assert!(deserialized.is_err());
            prop_assert_eq!(
                deserialized.unwrap_err().to_string(),
                "io error: failed to fill whole buffer"
            );
        } else {
            // Deserialization should have succeeded
            prop_assert!(deserialized.is_ok());

            // Reserialize inventory hash
            let mut bytes = Vec::new();
            let serialization_result = deserialized.unwrap().zcash_serialize(&mut bytes);

            prop_assert!(serialization_result.is_ok());

            // Check that the reserialization produces the same bytes as the input
            prop_assert!(bytes.len() <= input.len());
            prop_assert_eq!(&bytes, &input[..bytes.len()]);
        }
    }

    /// Test if a [`Message::{Inv, GetData}`] is not changed after encoding and decoding it.
    // TODO: Update this test to cover all `Message` variants.
    #[test]
    fn inv_and_getdata_message_roundtrip(
        message in prop_oneof!(Message::inv_strategy(), Message::get_data_strategy()),
    ) {
        let mut codec = Codec::builder().finish();
        let mut bytes = BytesMut::with_capacity(MAX_PROTOCOL_MESSAGE_LEN);

        let encoding_result = codec.encode(message.clone(), &mut bytes);

        prop_assert!(encoding_result.is_ok());

        let decoded: Result<Option<Message>, _> = codec.decode(&mut bytes);

        prop_assert!(decoded.is_ok());
        prop_assert_eq!(decoded.unwrap(), Some(message));
    }
}
