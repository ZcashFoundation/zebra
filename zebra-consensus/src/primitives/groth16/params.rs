//! Loading and checking correctness of Groth16 Sprout parameters.

use bellman::groth16::{prepare_verifying_key, PreparedVerifyingKey, VerifyingKey};
use bls12_381::Bls12;
use derive_getters::Getters;

lazy_static::lazy_static! {
    /// Spend parameters for the Sprout circuit.
    ///
    /// Used to verify Sprout shielded data in transactions.
    ///
    /// Note that adding value to the Sprout pool was disabled by the Canopy network upgrade.
    pub static ref SPROUT: SproutParams = Default::default();
}

/// Spend parameters for the Sprout circuit.
///
/// Used to verify Sprout shielded data in transactions.
///
/// Note that adding value to the Sprout pool was disabled by the Canopy network upgrade.
#[derive(Getters)]
pub struct SproutParams {
    prepared_verifying_key: PreparedVerifyingKey<Bls12>,
}

impl Default for SproutParams {
    fn default() -> Self {
        let sprout_vk = VerifyingKey::<Bls12>::read(&include_bytes!("sprout-groth16.vk")[..])
            .expect("should be able to read and parse Sprout verification key");

        Self {
            prepared_verifying_key: prepare_verifying_key(&sprout_vk),
        }
    }
}
