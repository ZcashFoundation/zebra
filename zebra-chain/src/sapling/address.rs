//! Shielded addresses.

use std::{
    convert::TryFrom,
    fmt,
    io::{self, Read, Write},
};

use bech32::{self, FromBase32, ToBase32, Variant};

#[cfg(test)]
use proptest::prelude::*;

use crate::{
    parameters::Network,
    serialization::{ReadZcashExt, SerializationError},
};

use super::keys;

/// Human-Readable Parts for input to bech32 encoding.
mod human_readable_parts {
    pub const MAINNET: &str = "zs";
    pub const TESTNET: &str = "ztestsapling";
}

/// A Sapling _shielded payment address_.
///
/// Also known as a _diversified payment address_ for Sapling, as
/// defined in [§4.2.2][4.2.2].
///
/// [4.2.2]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Address {
    network: Network,
    diversifier: keys::Diversifier,
    transmission_key: keys::TransmissionKey,
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SaplingAddress")
            .field("network", &self.network)
            .field("diversifier", &self.diversifier)
            .field("transmission_key", &self.transmission_key)
            .finish()
    }
}

impl fmt::Display for Address {
    #[allow(clippy::unwrap_in_result)]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());

        let _ = bytes.write_all(&<[u8; 11]>::from(self.diversifier));
        let _ = bytes.write_all(&<[u8; 32]>::from(self.transmission_key));

        let hrp = match self.network {
            Network::Mainnet => human_readable_parts::MAINNET,
            _ => human_readable_parts::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, bytes.get_ref().to_base32(), Variant::Bech32)
            .expect("hrp is valid")
    }
}

impl std::str::FromStr for Address {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes, Variant::Bech32)) => {
                let mut decoded_bytes =
                    io::Cursor::new(Vec::<u8>::from_base32(&bytes).map_err(|_| {
                        SerializationError::Parse("bech32::decode guarantees valid base32")
                    })?);

                let mut diversifier_bytes = [0; 11];
                decoded_bytes.read_exact(&mut diversifier_bytes)?;

                let transmission_key_bytes = decoded_bytes.read_32_bytes()?;

                Ok(Address {
                    network: match hrp.as_str() {
                        human_readable_parts::MAINNET => Network::Mainnet,
                        _ => Network::Testnet,
                    },
                    diversifier: keys::Diversifier::from(diversifier_bytes),
                    transmission_key: keys::TransmissionKey::try_from(transmission_key_bytes)
                        .map_err(|_| SerializationError::Parse("invalid transmission key bytes"))?,
                })
            }
            _ => Err(SerializationError::Parse("bech32 decoding error")),
        }
    }
}

#[cfg(test)]
impl Arbitrary for Address {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Network>(),
            any::<keys::Diversifier>(),
            any::<keys::TransmissionKey>(),
        )
            .prop_map(|(network, diversifier, transmission_key)| Self {
                network,
                diversifier,
                transmission_key,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod tests {

    use rand_core::OsRng;

    use super::*;

    #[test]
    fn from_str_display() {
        let _init_guard = zebra_test::init();

        let zs_addr: Address =
            "zs1qqqqqqqqqqqqqqqqqrjq05nyfku05msvu49mawhg6kr0wwljahypwyk2h88z6975u563j8nfaxd"
                .parse()
                .unwrap();

        assert_eq!(
            format!("{}", zs_addr),
            "zs1qqqqqqqqqqqqqqqqqrjq05nyfku05msvu49mawhg6kr0wwljahypwyk2h88z6975u563j8nfaxd"
        );
    }

    #[test]
    fn derive_keys_and_addresses() {
        let _init_guard = zebra_test::init();

        let spending_key = keys::SpendingKey::new(&mut OsRng);

        let spend_authorizing_key = keys::SpendAuthorizingKey::from(spending_key);
        let proof_authorizing_key = keys::ProofAuthorizingKey::from(spending_key);

        let authorizing_key = keys::AuthorizingKey::from(spend_authorizing_key);
        let nullifier_deriving_key = keys::NullifierDerivingKey::from(proof_authorizing_key);
        let incoming_viewing_key =
            keys::IncomingViewingKey::from((authorizing_key, nullifier_deriving_key));

        let diversifier = keys::Diversifier::new(&mut OsRng);
        let transmission_key = keys::TransmissionKey::try_from((incoming_viewing_key, diversifier))
            .expect("should be a valid transmission key");

        let _sapling_shielded_address = Address {
            network: Network::Mainnet,
            diversifier,
            transmission_key,
        };
    }
}

#[cfg(test)]
proptest! {

    #[test]
    fn sapling_address_roundtrip(zaddr in any::<Address>()) {
        let _init_guard = zebra_test::init();

        let string = zaddr.to_string();

        let zaddr2 = string.parse::<Address>()
            .expect("randomized sapling z-addr should deserialize");

        prop_assert_eq![zaddr, zaddr2];
    }
}
