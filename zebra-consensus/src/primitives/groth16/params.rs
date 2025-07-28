//! Loading and checking correctness of Groth16 Sapling and Sprout parameters.

use bellman::groth16::{prepare_verifying_key, PreparedVerifyingKey, VerifyingKey};
use bls12_381::Bls12;
use derive_getters::Getters;
use zcash_proofs::prover::LocalTxProver;

lazy_static::lazy_static! {
    /// Sapling prover containing spend and output params for the Sapling circuit.
    ///
    /// Used to:
    ///
    /// - construct Sapling outputs in coinbase txs, and
    /// - verify Sapling shielded data in the tx verifier.
    pub static ref SAPLING: LocalTxProver = LocalTxProver::bundled();

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
