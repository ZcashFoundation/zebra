//! Downloading, checking, and loading Groth16 Sapling and Sprout parameters.

use std::path::PathBuf;

use bellman::groth16;
use bls12_381::Bls12;

lazy_static::lazy_static! {
    /// Groth16 Zero-Knowledge Proof parameters for the Sapling and Sprout circuits.
    ///
    /// When this static is accessed:
    /// - the parameters are downloded if needed, then cached to a shared directory,
    /// - the file hashes are checked, for both newly downloaded and previously cached files,
    /// - the parameters are loaded into Zebra.
    ///
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    pub static ref GROTH16_PARAMETERS: Groth16Parameters = Groth16Parameters::new();
}

/// Groth16 Zero-Knowledge Proof parameters for the Sapling and Sprout circuits.
#[non_exhaustive]
pub struct Groth16Parameters {
    /// The Sapling circuit Groth16 parameters.
    pub sapling: SaplingParameters,
}

impl Groth16Parameters {
    /// Download if needed, cache, check, and load the Sprout and Sapling Groth16 parameters.
    ///
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    fn new() -> Groth16Parameters {
        Groth16Parameters {
            sapling: SaplingParameters::new(),
        }
    }

    /// Returns the path to the Groth16 parameters directory.
    pub fn directory() -> PathBuf {
        zcash_proofs::default_params_folder().expect("unable to find user home directory")
    }

    /// Returns a hint that helps users recover from parameter download failures.
    pub fn failure_hint() -> String {
        format!(
            "Hint: try deleting {:?}, then running 'zebrad download' to re-download the parameters",
            Groth16Parameters::directory(),
        )
    }
}

/// Groth16 Zero-Knowledge Proof spend and output parameters for the Sapling circuit.
#[non_exhaustive]
pub struct SaplingParameters {
    pub spend: groth16::Parameters<Bls12>,
    pub spend_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,

    pub output: groth16::Parameters<Bls12>,
    pub output_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
}

impl SaplingParameters {
    /// Download if needed, cache, check, and load the Sapling Groth16 parameters.
    ///
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    fn new() -> SaplingParameters {
        // TODO: Sprout

        let params_directory = Groth16Parameters::directory();
        let spend_path = params_directory.join("sapling-spend.params");
        let output_path = params_directory.join("sapling-output.params");

        // Download parameters if needed.
        //
        // TODO: use try_exists when it stabilises, to exit early on permissions errors (#83186)
        if !spend_path.exists() || !output_path.exists() {
            tracing::info!("downloading Zcash Sapling parameters");
            zcash_proofs::download_parameters().unwrap_or_else(|_| {
                panic!(
                    "error downloading parameter files. {}",
                    Groth16Parameters::failure_hint()
                )
            });
        }

        // TODO: if loading fails, log a message including `failure_hint`
        tracing::info!("checking and loading Zcash Sapling parameters");
        let parameters = zcash_proofs::load_parameters(&spend_path, &output_path, None);

        SaplingParameters {
            spend: parameters.spend_params,
            spend_prepared_verifying_key: parameters.spend_vk,
            output: parameters.output_params,
            output_prepared_verifying_key: parameters.output_vk,
        }
    }
}
