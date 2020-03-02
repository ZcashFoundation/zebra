//! Address types.

use secp256k1::PublicKey;

use crate::serialization::{
    ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
};

use crate::types::Script;

/// A hash of a redeem script, as used in transparent
/// pay-to-script-hash addresses.
///
/// https://en.bitcoin.it/Base58Check_encoding#Encoding_a_Bitcoin_address
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(test, derive(Arbitrary))]
struct AddressPayloadHash(pub [u8; 20]);

impl From<Script> for AddressPayloadHash {
    fn from(script: Script) -> Self {
        use ripemd160::Ripdemd160;
        use sha2::Sha256;

        let hash = Ripdemd160::digest(Sha256::digest(&script.0[..]));

        Self(hash)
    }
}

impl From<PublicKey> for AddressPayloadHash {
    fn from(publickey: PublicKey) -> Self {
        use ripemd160::Ripdemd160;
        use sha2::Sha256;

        let hash = Ripdemd160::digest(Sha256::digest(&publickey.0[..]));

        Self(hash)
    }
}

impl fmt::Debug for AddressPayloadHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("AddressPayloadHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

pub enum TransparentAddress {
    PayToScriptHash,
    PayToPublicKeyHash,
}

impl ZcashSerialize for Transaction {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        unimplemented!();
    }
}

impl ZcashDeserialize for Transaction {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}
