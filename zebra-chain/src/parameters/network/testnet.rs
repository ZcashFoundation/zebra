//! Types and implementation for Testnet consensus parameters

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// Network consensus parameters for test networks such as Regtest and the default Testnet.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Parameters {}

impl Parameters {
    /// Returns true if the instance of [`Parameters`] represents the default public Testnet.
    pub fn is_default_testnet(&self) -> bool {
        self == &Self::default()
    }
}
