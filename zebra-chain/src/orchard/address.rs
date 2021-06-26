//! Orchard shielded payment addresses.

use std::fmt;

#[cfg(test)]
use proptest::prelude::*;

use super::keys;

/// A raw **Orchard** _shielded payment address_.
///
/// Also known as a _diversified payment address_ for Orchard, as
/// defined in [§5.6.4.1 of the Zcash Specification][orchardpaymentaddrencoding].
///
/// [orchardpaymentaddrencoding]: https://zips.z.cash/protocol/nu5.pdf#orchardpaymentaddrencoding
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Address {
    pub(crate) diversifier: keys::Diversifier,
    pub(crate) transmission_key: keys::TransmissionKey,
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("OrchardAddress")
            .field("diversifier", &self.diversifier)
            .field("transmission_key", &self.transmission_key)
            .finish()
    }
}

#[cfg(test)]
impl Arbitrary for Address {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (any::<keys::Diversifier>(), any::<keys::TransmissionKey>())
            .prop_map(|(diversifier, transmission_key)| Self {
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

    use crate::parameters::Network;

    use super::*;

    #[test]
    fn derive_keys_and_addresses() {
        zebra_test::init();

        let network = Network::Mainnet;

        let spending_key = keys::SpendingKey::new(&mut OsRng, network);

        let full_viewing_key = keys::FullViewingKey::from_spending_key(spending_key);

        // Default diversifier, where index = 0.
        let diversifier_key = keys::DiversifierKey::from(full_viewing_key);

        let incoming_viewing_key = keys::IncomingViewingKey::from(full_viewing_key);

        let diversifier = keys::Diversifier::from(diversifier_key);
        let transmission_key = keys::TransmissionKey::from((incoming_viewing_key, diversifier));

        let _orchard_shielded_address = Address {
            diversifier,
            transmission_key,
        };
    }
}
