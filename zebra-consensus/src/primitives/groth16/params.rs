//! Downloading, checking, and loading Groth16 Sapling and Sprout parameters.

use std::path::PathBuf;

use bellman::groth16;
use bls12_381::Bls12;

/// The timeout for each parameter file download, in seconds.
///
/// Zebra assumes that it's running on at least a 10 Mbps connection.
/// So the parameter files should download in about 15 minutes using `zebrad download`.
/// But `zebrad start` downloads blocks at the same time, so we allow some extra time.
pub const PARAMETER_DOWNLOAD_TIMEOUT: u64 = 60 * 60;

/// The maximum number of times to retry download parameters.
///
/// Zebra will retry to download Sprout of Sapling parameters only if they
/// failed for whatever reason.
pub const PARAMETER_DOWNLOAD_MAX_RETRIES: usize = 3;

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
/// New Sprout outputs were disabled by the Canopy network upgrade.
pub struct SproutParameters {
    pub joinsplit_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
}

impl Groth16Parameters {
    /// Download if needed, cache, check, and load the Sprout and Sapling Groth16 parameters.
    ///
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    fn new() -> Groth16Parameters {
        let params_directory = Groth16Parameters::directory();
        let sapling_spend_path = params_directory.join(zcash_proofs::SAPLING_SPEND_NAME);
        let sapling_output_path = params_directory.join(zcash_proofs::SAPLING_OUTPUT_NAME);
        let sprout_path = params_directory.join(zcash_proofs::SPROUT_NAME);

        // TODO: instead of the path check, add a zcash_proofs argument to skip hashing existing files
        //       (we check them on load anyway)
        if !sapling_spend_path.exists() || !sapling_output_path.exists() {
            tracing::info!("downloading Zcash Sapling parameters");

            let mut sapling_retries = 0;
            while sapling_retries < PARAMETER_DOWNLOAD_MAX_RETRIES {
                let new_sapling_paths =
                    zcash_proofs::download_sapling_parameters(Some(PARAMETER_DOWNLOAD_TIMEOUT));

                if new_sapling_paths.is_err() {
                    sapling_retries += 1;
                    tracing::info!("error downloading Zcash Sapling parameters, retrying");
                } else {
                    assert_eq!(
                        sapling_spend_path,
                        new_sapling_paths.as_ref().unwrap().spend
                    );
                    assert_eq!(sapling_output_path, new_sapling_paths.unwrap().output);
                    break;
                }
            }
            if sapling_retries == PARAMETER_DOWNLOAD_MAX_RETRIES {
                panic!(
                    "error downloading Sapling parameter files after {} retries. {}",
                    PARAMETER_DOWNLOAD_MAX_RETRIES,
                    Groth16Parameters::failure_hint()
                );
            }
        }

        if !sprout_path.exists() {
            tracing::info!("downloading Zcash Sprout parameters");

            let mut sprout_retries = 0;
            while sprout_retries < PARAMETER_DOWNLOAD_MAX_RETRIES {
                let new_sprout_path =
                    zcash_proofs::download_sprout_parameters(Some(PARAMETER_DOWNLOAD_TIMEOUT));

                if new_sprout_path.is_err() {
                    sprout_retries += 1;
                    tracing::info!("error downloading Zcash Sprout parameters, retrying");
                } else {
                    assert_eq!(sprout_path, new_sprout_path.unwrap());
                    break;
                }
            }
            if sprout_retries == PARAMETER_DOWNLOAD_MAX_RETRIES {
                panic!(
                    "error downloading Sprout parameter files after {} retries. {}",
                    PARAMETER_DOWNLOAD_MAX_RETRIES,
                    Groth16Parameters::failure_hint()
                );
            }
        }

        // TODO: if loading fails, log a message including `failure_hint`
        tracing::info!("checking and loading Zcash Sapling and Sprout parameters");
        let parameters = zcash_proofs::load_parameters(
            &sapling_spend_path,
            &sapling_output_path,
            Some(&sprout_path),
        );

        let sapling = SaplingParameters {
            spend: parameters.spend_params,
            spend_prepared_verifying_key: parameters.spend_vk,
            output: parameters.output_params,
            output_prepared_verifying_key: parameters.output_vk,
        };

        let sprout = SproutParameters {
            joinsplit_prepared_verifying_key: parameters
                .sprout_vk
                .expect("unreachable code: sprout loader panics on failure"),
        };

        Groth16Parameters { sapling, sprout }
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
