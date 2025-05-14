//! Network `Magic` type and implementation.

use std::fmt;

use crate::parameters::{constants::magics, Network};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A magic number identifying the network.
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Magic(pub [u8; 4]);

impl fmt::Debug for Magic {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Magic").field(&hex::encode(self.0)).finish()
    }
}

impl Network {
    /// Get the magic value associated to this `Network`.
    pub fn magic(&self) -> Magic {
        match self {
            Network::Mainnet => magics::MAINNET,
            Network::Testnet(params) => params.network_magic(),
        }
    }
}

#[cfg(test)]
mod magic_proptest {

    use proptest::prelude::*;

    use super::{magics, Magic};

    #[test]
    fn magic_debug() {
        let _init_guard = zebra_test::init();

        assert_eq!(format!("{:?}", magics::MAINNET), "Magic(\"24e92764\")");
        assert_eq!(format!("{:?}", magics::TESTNET), "Magic(\"fa1af9bf\")");
        assert_eq!(format!("{:?}", magics::REGTEST), "Magic(\"aae83f5f\")");
    }

    proptest! {

        #[test]
        fn proptest_magic_from_array(data in any::<[u8; 4]>()) {
            let _init_guard = zebra_test::init();

            assert_eq!(format!("{:?}", Magic(data)), format!("Magic({:x?})", hex::encode(data)));
        }
    }
}
