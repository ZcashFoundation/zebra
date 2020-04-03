//! Sapling Shielded Payment Address types.

use std::{fmt, io};

use bech32::{self, FromBase32, ToBase32};

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, array, prelude::*};

use crate::{
    keys::sapling,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
    Network,
};

/// Human-Readable Parts for input to bech32 encoding.
mod human_readable_parts {
    pub const MAINNET: &str = "zs";
    pub const TESTNET: &str = "ztestsapling";
}

///
#[derive(Clone, Copy, PartialEq)]
pub struct SaplingShieldedAddress {
    diversifier: sapling::Diversifier,
    transmission_key: sapling::TransmissionKey,
}

impl fmt::Debug for SaplingShieldedAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SaplingShieldedAddress")
            .field("diversifier", &self.diversifier)
            .field("transmission_key", &self.transmission_key)
            .finish()
    }
}

impl SaplingShieldedAddress {
    fn to_human_readable_address(&self, network: Network) -> Result<String, bech32::Error> {
        let mut bytes = io::Cursor::new(Vec::new());
        let _ = self.zcash_serialize(&mut bytes);

        let mut hrp = "";

        match network {
            Network::Mainnet => hrp = human_readable_parts::MAINNET,
            _ => hrp = human_readable_parts::TESTNET,
        }

        bech32::encode(hrp, bytes.get_ref().to_base32())
    }
}

impl Eq for SaplingShieldedAddress {}

impl ZcashSerialize for SaplingShieldedAddress {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.diversifier.0[..])?;
        writer.write_all(&self.transmission_key.to_bytes())?;

        Ok(())
    }
}

impl ZcashDeserialize for SaplingShieldedAddress {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut diversifier_bytes = [0; 11];
        reader.read_exact(&mut diversifier_bytes)?;

        let transmission_key_bytes = reader.read_32_bytes()?;

        Ok(SaplingShieldedAddress {
            diversifier: sapling::Diversifier(diversifier_bytes),
            transmission_key: sapling::TransmissionKey::from_bytes(transmission_key_bytes),
        })
    }
}

impl std::str::FromStr for SaplingShieldedAddress {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((_, bytes)) => {
                let decoded = Vec::<u8>::from_base32(&bytes).unwrap();
                Self::zcash_deserialize(io::Cursor::new(decoded))
            }
            Err(_) => Err(SerializationError::Parse("bech32 decoding error")),
        }
    }
}

#[cfg(test)]
impl Arbitrary for SaplingShieldedAddress {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (array::uniform11(any::<u8>()), array::uniform32(any::<u8>()))
            .prop_map(|(diversifier_bytes, transmission_key_bytes)| {
                return Self {
                    diversifier: sapling::Diversifier(diversifier_bytes),
                    transmission_key: sapling::TransmissionKey::from_bytes(transmission_key_bytes),
                };
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod tests {

    use std::str::FromStr;

    use super::*;

    #[test]
    fn from_str_debug() {
        let zs_addr = SaplingShieldedAddress::from_str(
            "zs1qqqqqqqqqqqqqqqqqrjq05nyfku05msvu49mawhg6kr0wwljahypwyk2h88z6975u563j8nfaxd",
        )
        .expect("sapling z-addr string to parse");

        assert_eq!(
            format!("{:?}", zs_addr),

            "SaplingShieldedAddress { diversifier: Diversifier(\"0000000000000000000000\"), transmission_key: TransmissionKey { u: \"8aa95397aad733606e2cd16834f2ee5b537dad92b1ad0529653ba6a9772b8c03\", v: \"e407d2644db8fa6e0ce54bbebae8d586f73bf2edc81712cab9ce2d17d4e53519\" } }"
        );
    }

    #[test]
    fn to_human_readable_address() {
        let zs_addr = SaplingShieldedAddress::from_str(
            "zs1qqqqqqqqqqqqqqqqqrjq05nyfku05msvu49mawhg6kr0wwljahypwyk2h88z6975u563j8nfaxd",
        )
        .expect("sapling z-addr string to parse");

        let address = zs_addr
            .to_human_readable_address(Network::Mainnet)
            .expect("z-addr to serialize in a human-readable way");

        assert_eq!(
            format!("{}", address),
            "zs1qqqqqqqqqqqqqqqqqrjq05nyfku05msvu49mawhg6kr0wwljahypwyk2h88z6975u563j8nfaxd"
        );
    }
}

#[cfg(test)]
proptest! {

    // #[test]
    // fn sapling_address_roundtrip(zaddr in any::<SaplingShieldedAddress>()) {

    //     let mut data = Vec::new();

    //     zaddr.zcash_serialize(&mut data).expect("sapling z-addr should serialize");

    //     let zaddr2 = SaplingShieldedAddress::zcash_deserialize(&data[..])
    //         .expect("randomized sapling z-addr should deserialize");

    //     prop_assert_eq![zaddr, zaddr2];
    // }
}
