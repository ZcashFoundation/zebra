//! Loading and checking correctness of Groth16 Sapling and Sprout parameters.

use bellman::groth16::{prepare_verifying_key, Parameters, PreparedVerifyingKey, VerifyingKey};
use bls12_381::Bls12;
use zcash_proofs::prover::LocalTxProver;

lazy_static::lazy_static! {
    /// Spend and output parameters for the Sapling circuit.
    ///
    /// Used to construct Sapling outputs in coinbase txs.
    pub static ref PROVER_PARAMS: LocalTxProver = LocalTxProver::bundled().clone();

    /// Groth16 Zero-Knowledge Proof parameters for the Sapling and Sprout circuits.
    ///
    /// This static is accessed when Zebra needs to use these parameters for verification.
    pub static ref GROTH16_PARAMETERS: Groth16Parameters = Groth16Parameters::new(PROVER_PARAMS.clone());
}

/// Groth16 Zero-Knowledge Proof parameters for the Sapling and Sprout circuits.
pub struct Groth16Parameters {
    /// The Sapling circuit Groth16 parameters.
    pub sapling: SaplingParameters,
    /// The Sprout circuit Groth16 spend parameters.
    pub sprout: SproutParameters,
}

/// Groth16 Zero-Knowledge Proof spend and output parameters for the Sapling circuit.
pub struct SaplingParameters {
    pub spend: Parameters<Bls12>,
    pub spend_prepared_verifying_key: PreparedVerifyingKey<Bls12>,

    pub output: Parameters<Bls12>,
    pub output_prepared_verifying_key: PreparedVerifyingKey<Bls12>,
}

/// Groth16 Zero-Knowledge Proof spend parameters for the Sprout circuit.
///
/// Adding value to the Sprout pool was disabled by the Canopy network upgrade.
pub struct SproutParameters {
    pub joinsplit_prepared_verifying_key: PreparedVerifyingKey<Bls12>,
}

impl Groth16Parameters {
    /// Loads the Groth16 parameters for Sapling and Sprout circuits.
    fn new(prover_params: LocalTxProver) -> Groth16Parameters {
        let sprout_vk = VerifyingKey::<Bls12>::read(&include_bytes!("sprout-groth16.vk")[..])
            .expect("should be able to read and parse Sprout verification key");

        let (spend_params, output_params) = prover_params.params();
        let (spend, output) = (spend_params.inner(), output_params.inner());
        let (spend_prepared_verifying_key, output_prepared_verifying_key) = (
            prepare_verifying_key(&spend.vk),
            prepare_verifying_key(&output.vk),
        );

        Groth16Parameters {
            sapling: SaplingParameters {
                spend,
                spend_prepared_verifying_key,
                output,
                output_prepared_verifying_key,
            },
            sprout: SproutParameters {
                joinsplit_prepared_verifying_key: prepare_verifying_key(&sprout_vk),
            },
        }
    }
}
