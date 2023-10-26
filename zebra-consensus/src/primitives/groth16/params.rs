//! Loading and checking correctness of Groth16 Sapling and Sprout parameters.

use bellman::groth16;
use bls12_381::Bls12;

lazy_static::lazy_static! {
    /// Groth16 Zero-Knowledge Proof parameters for the Sapling and Sprout circuits.
    ///
    /// This static is accessed when Zebra needs to use these parameters for verification.
    ///
    /// # Panics
    ///
    /// If the parameter data in the `zebrad` binary is invalid.
    pub static ref GROTH16_PARAMETERS: Groth16Parameters = Groth16Parameters::new();
}

/// Groth16 Zero-Knowledge Proof parameters for the Sapling and Sprout circuits.
pub struct Groth16Parameters {
    /// The Sapling circuit Groth16 parameters.
    pub sapling: SaplingParameters,

    /// The Sprout circuit Groth16 spend parameter.
    pub sprout: SproutParameters,
}

/// Groth16 Zero-Knowledge Proof spend and output parameters for the Sapling circuit.
pub struct SaplingParameters {
    pub spend: groth16::Parameters<Bls12>,
    pub spend_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,

    pub output: groth16::Parameters<Bls12>,
    pub output_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
}

/// Groth16 Zero-Knowledge Proof spend parameters for the Sprout circuit.
///
/// Adding value to the Sprout pool was disabled by the Canopy network upgrade.
pub struct SproutParameters {
    pub joinsplit_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
}

impl Groth16Parameters {
    /// Loads the Sprout and Sapling Groth16 parameters from the `zebrad` binary, and checks that
    /// the data is valid.
    ///
    /// # Panics
    ///
    /// If the parameter data in the `zebrad` binary is invalid.
    fn new() -> Groth16Parameters {
        tracing::info!("checking and loading Zcash Sapling and Sprout parameters");

        let (sapling_spend_bytes, sapling_output_bytes) =
            wagyu_zcash_parameters::load_sapling_parameters();
        let sprout_vk_bytes = include_bytes!("sprout-groth16.vk");

        let sapling_parameters = zcash_proofs::parse_parameters(
            sapling_spend_bytes.as_slice(),
            sapling_output_bytes.as_slice(),
            // This API expects the full sprout parameter file, not just the verifying key.
            None,
        );

        let sapling = SaplingParameters {
            spend: sapling_parameters.spend_params,
            spend_prepared_verifying_key: sapling_parameters.spend_vk,
            output: sapling_parameters.output_params,
            output_prepared_verifying_key: sapling_parameters.output_vk,
        };

        let sprout_vk = groth16::VerifyingKey::<Bls12>::read(&sprout_vk_bytes[..])
            .expect("should be able to parse Sprout verification key");
        let sprout_vk = groth16::prepare_verifying_key(&sprout_vk);

        let sprout = SproutParameters {
            joinsplit_prepared_verifying_key: sprout_vk,
        };

        tracing::info!("Zcash Sapling and Sprout parameters loaded and verified");

        Groth16Parameters { sapling, sprout }
    }

    /// Returns a hint that helps users recover from parameter loading failures.
    pub fn failure_hint() -> String {
        "Hint: re-run `zebrad` or re-install it from a trusted source".to_string()
    }
}
