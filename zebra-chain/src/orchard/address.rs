//! Orchard shielded payment addresses.

use std::fmt;

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
