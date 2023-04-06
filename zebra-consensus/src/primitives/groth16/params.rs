//! Downloading, checking, and loading Groth16 Sapling and Sprout parameters.

use std::path::{Path, PathBuf};

use bellman::groth16;
use bls12_381::Bls12;
use zcash_proofs::SaplingParameterPaths;

use crate::BoxError;

/// The timeout for each parameter file download, in seconds.
///
/// Zebra assumes that it's running on at least a 10 Mbps connection.
/// So the parameter files should download in about 15 minutes using `zebrad download`.
/// But `zebrad start` downloads blocks at the same time, so we allow some extra time.
pub const PARAMETER_DOWNLOAD_TIMEOUT: u64 = 60 * 60;

/// The maximum number of times Zebra retries to download the parameters.
///
/// Zebra will retry the download only if the previous attempt fails.
pub const PARAMETER_DOWNLOAD_MAX_RETRIES: usize = 2;

lazy_static::lazy_static! {
    /// Groth16 Zero-Knowledge Proof parameters for the Sapling and Sprout circuits.
    ///
    /// This static is accessed when Zebra:
    ///
    /// - needs to download and cache the parameters to a shared directory, or
    /// - checks the file hashes for both newly downloaded and previously cached files, or
    /// - loads the parameters.
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
    /// Loads the Sprout and Sapling Groth16 parameters, checking the sizes and hashes of the files.
    ///
    /// If the parameters are not present, they are automatically downloaded and cached.
    ///
    /// # Panics
    ///
    /// - If the parameters were downloaded to a wrong path.
    /// - After [`PARAMETER_DOWNLOAD_MAX_RETRIES`] failed download retries.
    /// - If the downloaded or pre-existing parameter files are invalid.
    fn new() -> Groth16Parameters {
        let params_directory = Groth16Parameters::directory();
        let sapling_spend_path = params_directory.join(zcash_proofs::SAPLING_SPEND_NAME);
        let sapling_output_path = params_directory.join(zcash_proofs::SAPLING_OUTPUT_NAME);
        let sprout_path = params_directory.join(zcash_proofs::SPROUT_NAME);

        Groth16Parameters::download_sapling_parameters(&sapling_spend_path, &sapling_output_path);
        Groth16Parameters::download_sprout_parameters(&sprout_path);

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

        tracing::info!("Zcash Sapling and Sprout parameters downloaded and/or verified");

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

    /// Downloads the Sapling parameters.
    ///
    /// # Panics
    ///
    /// - If the parameters were downloaded to a wrong path.
    /// - After [`PARAMETER_DOWNLOAD_MAX_RETRIES`] failed download retries.
    fn download_sapling_parameters(sapling_spend_path: &Path, sapling_output_path: &Path) {
        // TODO: instead of the path check, add a zcash_proofs argument to skip hashing existing files
        //       (we check them on load anyway)
        if !sapling_spend_path.exists() || !sapling_output_path.exists() {
            tracing::info!("downloading Zcash Sapling parameters");

            let mut retries = 0;
            while let Err(error) = Groth16Parameters::download_sapling_parameters_once(
                sapling_spend_path,
                sapling_output_path,
            ) {
                retries += 1;
                if retries > PARAMETER_DOWNLOAD_MAX_RETRIES {
                    panic!(
                        "error downloading Sapling parameter files after {} retries. {:?} {}",
                        PARAMETER_DOWNLOAD_MAX_RETRIES,
                        error,
                        Groth16Parameters::failure_hint(),
                    );
                } else {
                    tracing::info!(
                        ?error,
                        "error downloading Zcash Sapling parameters, retrying"
                    );
                }
            }
        }
    }

    /// Tries to download the Sapling parameters once and returns the result.
    ///
    /// # Panics
    ///
    /// If the parameters were downloaded to paths different to `sapling_spend_path` or
    /// `sapling_output_path`.
    fn download_sapling_parameters_once(
        sapling_spend_path: &Path,
        sapling_output_path: &Path,
    ) -> Result<SaplingParameterPaths, BoxError> {
        let new_sapling_paths =
            zcash_proofs::download_sapling_parameters(Some(PARAMETER_DOWNLOAD_TIMEOUT))?;

        assert_eq!(sapling_spend_path, new_sapling_paths.spend);
        assert_eq!(sapling_output_path, new_sapling_paths.output);

        Ok(new_sapling_paths)
    }

    /// Downloads the Sprout parameters.
    ///
    /// # Panics
    ///
    /// - If the parameters were downloaded to a wrong path.
    /// - After [`PARAMETER_DOWNLOAD_MAX_RETRIES`] failed download retries.
    fn download_sprout_parameters(sprout_path: &Path) {
        if !sprout_path.exists() {
            tracing::info!("downloading Zcash Sprout parameters");

            let mut retries = 0;
            while let Err(error) = Groth16Parameters::download_sprout_parameters_once(sprout_path) {
                retries += 1;
                if retries > PARAMETER_DOWNLOAD_MAX_RETRIES {
                    panic!(
                        "error downloading Sprout parameter files after {} retries. {:?} {}",
                        PARAMETER_DOWNLOAD_MAX_RETRIES,
                        error,
                        Groth16Parameters::failure_hint(),
                    );
                } else {
                    tracing::info!(
                        ?error,
                        "error downloading Zcash Sprout parameters, retrying"
                    );
                }
            }
        }
    }

    /// Tries to download the Sprout parameters once and returns the result.
    ///
    /// # Panics
    ///
    /// If the parameters were downloaded to a path different to `sprout_path`.
    fn download_sprout_parameters_once(sprout_path: &Path) -> Result<PathBuf, BoxError> {
        let new_sprout_path =
            zcash_proofs::download_sprout_parameters(Some(PARAMETER_DOWNLOAD_TIMEOUT))?;

        assert_eq!(sprout_path, new_sprout_path);

        Ok(new_sprout_path)
    }
}
