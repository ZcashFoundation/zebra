//! Orchard shielded payment addresses.

use std::{
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
    pub const MAINNET: &str = "zo";
    pub const TESTNET: &str = "ztestorchard";
}

/// A Orchard _shielded payment address_.
///
/// Also known as a _diversified payment address_ for Orchard, as
/// defined in [§5.6.4.1 of the Zcash Specification][orchardpaymentaddrencoding].
///
/// [orchardpaymentaddrencoding]: https://zips.z.cash/protocol/nu5.pdf#orchardpaymentaddrencoding
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Address {
    network: Network,
    diversifier: keys::Diversifier,
    transmission_key: keys::TransmissionKey,
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("OrchardAddress")
            .field("network", &self.network)
            .field("diversifier", &self.diversifier)
            .field("transmission_key", &self.transmission_key)
            .finish()
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());

        let _ = bytes.write_all(&<[u8; 11]>::from(self.diversifier));
        let _ = bytes.write_all(&<[u8; 32]>::from(self.transmission_key));

        let hrp = match self.network {
            Network::Mainnet => human_readable_parts::MAINNET,
            _ => human_readable_parts::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, bytes.get_ref().to_base32(), Variant::Bech32).unwrap()
    }
}

impl std::str::FromStr for Address {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes, Variant::Bech32)) => {
                let mut decoded_bytes = io::Cursor::new(Vec::<u8>::from_base32(&bytes).unwrap());

                let mut diversifier_bytes = [0; 11];
                decoded_bytes.read_exact(&mut diversifier_bytes)?;

                let transmission_key_bytes = decoded_bytes.read_32_bytes()?;

                Ok(Address {
                    network: match hrp.as_str() {
                        human_readable_parts::MAINNET => Network::Mainnet,
                        human_readable_parts::TESTNET => Network::Testnet,
                        _ => Err(SerializationError::Parse("unknown network")),
                    },
                    diversifier: keys::Diversifier::from(diversifier_bytes),
                    transmission_key: keys::TransmissionKey::from(transmission_key_bytes),
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

    // #[test]
    // fn from_str_display() {
    //     zebra_test::init();

    //     let zo_addr: Address =
    //         "zs1qqqqqqqqqqqqqqqqqrjq05nyfku05msvu49mawhg6kr0wwljahypwyk2h88z6975u563j8nfaxd"
    //             .parse()
    //             .unwrap();

    //     assert_eq!(
    //         format!("{}", zo_addr),
    //         "zs1qqqqqqqqqqqqqqqqqrjq05nyfku05msvu49mawhg6kr0wwljahypwyk2h88z6975u563j8nfaxd"
    //     );
    // }

    #[test]
    fn derive_keys_and_addresses() {
        zebra_test::init();

        let spending_key = keys::SpendingKey::new(&mut OsRng);

        let full_viewing_key = keys::FullViewingKey::from(spending_key);

        // Default diversifier, where index = 0.
        let diversifier_key = keys::DiversifierKey::from(full_viewing_key);

        let incoming_viewing_key = keys::IncomingViewingKey::from(full_viewing_key);

        let diversifier = keys::Diversifier::from(diversifier_key);
        let transmission_key = keys::TransmissionKey::from((incoming_viewing_key, diversifier));

        let _orchard_shielded_address = Address {
            network: Network::Mainnet,
            diversifier,
            transmission_key,
        };
    }
}

#[cfg(test)]
proptest! {

    #[test]
    fn orchard_address_roundtrip(zaddr in any::<Address>()) {
        zebra_test::init();

        let string = zaddr.to_string();

        let zaddr2 = string.parse::<Address>()
            .expect("randomized orchard z-addr should deserialize");

        prop_assert_eq![zaddr, zaddr2];
    }
}
