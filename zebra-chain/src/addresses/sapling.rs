//! Sapling Shielded Payment Address types.

use std::{
    fmt,
    io::{self, Read, Write},
};

use bech32::{self, FromBase32, ToBase32};

#[cfg(test)]
use proptest::prelude::*;

use crate::{
    keys::sapling,
    serialization::{ReadZcashExt, SerializationError},
    Network,
};

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
pub struct SaplingShieldedAddress {
    network: Network,
    diversifier: sapling::Diversifier,
    transmission_key: sapling::TransmissionKey,
}

impl fmt::Debug for SaplingShieldedAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SaplingShieldedAddress")
            .field("network", &self.network)
            .field("diversifier", &self.diversifier)
            .field("transmission_key", &self.transmission_key)
            .finish()
    }
}

impl fmt::Display for SaplingShieldedAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());

        let _ = bytes.write_all(&self.diversifier.0[..]);
        let _ = bytes.write_all(&self.transmission_key.to_bytes());

        let hrp = match self.network {
            Network::Mainnet => human_readable_parts::MAINNET,
            _ => human_readable_parts::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, bytes.get_ref().to_base32()).unwrap()
    }
}

impl std::str::FromStr for SaplingShieldedAddress {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes)) => {
                let mut decoded_bytes = io::Cursor::new(Vec::<u8>::from_base32(&bytes).unwrap());

                let mut diversifier_bytes = [0; 11];
                decoded_bytes.read_exact(&mut diversifier_bytes)?;

                let transmission_key_bytes = decoded_bytes.read_32_bytes()?;

                Ok(SaplingShieldedAddress {
                    network: match hrp.as_str() {
                        human_readable_parts::MAINNET => Network::Mainnet,
                        _ => Network::Testnet,
                    },
                    diversifier: sapling::Diversifier(diversifier_bytes),
                    transmission_key: sapling::TransmissionKey::from_bytes(transmission_key_bytes),
                })
            }
            Err(_) => Err(SerializationError::Parse("bech32 decoding error")),
        }
    }
}

#[cfg(test)]
impl Arbitrary for SaplingShieldedAddress {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Network>(),
            any::<sapling::Diversifier>(),
            any::<sapling::TransmissionKey>(),
        )
            .prop_map(|(network, diversifier, transmission_key)| {
                return Self {
                    network,
                    diversifier,
                    transmission_key,
                };
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod tests {

    use rand_core::OsRng;
    use std::str::FromStr;

    use super::*;

    #[test]
    fn from_str_display() {
        let zs_addr = SaplingShieldedAddress::from_str(
            "zs1qqqqqqqqqqqqqqqqqrjq05nyfku05msvu49mawhg6kr0wwljahypwyk2h88z6975u563j8nfaxd",
        )
        .expect("sapling z-addr string to parse");

        let address = zs_addr.to_string();

        assert_eq!(
            format!("{}", address),
            "zs1qqqqqqqqqqqqqqqqqrjq05nyfku05msvu49mawhg6kr0wwljahypwyk2h88z6975u563j8nfaxd"
        );
    }

    #[test]
    fn derive_keys_and_addresses() {
        let spending_key = sapling::SpendingKey::new(&mut OsRng);

        let spend_authorizing_key = sapling::SpendAuthorizingKey::from(spending_key);
        let proof_authorizing_key = sapling::ProofAuthorizingKey::from(spending_key);

        let authorizing_key = sapling::AuthorizingKey::from(spend_authorizing_key);
        let nullifier_deriving_key = sapling::NullifierDerivingKey::from(proof_authorizing_key);
        let incoming_viewing_key =
            sapling::IncomingViewingKey::from_keys(authorizing_key, nullifier_deriving_key);

        let diversifier = sapling::Diversifier::new(&mut OsRng);
        let transmission_key = sapling::TransmissionKey::from(incoming_viewing_key, diversifier);

        let _sapling_shielded_address = SaplingShieldedAddress {
            network: Network::Mainnet,
            diversifier,
            transmission_key,
        };
    }
}

#[cfg(test)]
proptest! {

    #[test]
    fn sapling_address_roundtrip(zaddr in any::<SaplingShieldedAddress>()) {

        let string = zaddr.to_string();

        let zaddr2 = string.parse::<SaplingShieldedAddress>()
            .expect("randomized sapling z-addr should deserialize");

        prop_assert_eq![zaddr, zaddr2];
    }
}
