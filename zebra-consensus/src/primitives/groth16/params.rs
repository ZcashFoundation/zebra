//! Downloading, verifying, and loading Groth16 Sapling and Sprout parameters.

use bellman::groth16;
use bls12_381::Bls12;

lazy_static::lazy_static! {
    pub static ref PARAMS: Groth16Params = Groth16Params::new();
}

/// Groth16 Zero-Knowledge Proof parameters for the Sapling and Sprout circuits.
///
/// Use [`Groth16Params::new`] to download the parameters if needed,
/// check they were downloaded correctly, and load them into Zebra.
#[non_exhaustive]
pub struct Groth16Params {
    /// The Sapling circuit Groth16 parameters.
    pub sapling: SaplingParams,
}

impl Default for Groth16Params {
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    fn default() -> Self {
        Self::new()
    }
}

impl Groth16Params {
    /// Download the Sapling and Sprout Groth16 parameters if needed,
    /// check they were downloaded correctly, and load them into Zebra.
    ///
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    pub fn new() -> Groth16Params {
        Groth16Params {
            sapling: SaplingParams::new(),
        }
    }
}

/// Groth16 Zero-Knowledge Proof spend and output parameters for the Sapling circuit.
///
/// Use [`SaplingParams::new`] to download the parameters if needed,
/// check they were downloaded correctly, and load them into Zebra.
#[non_exhaustive]
pub struct SaplingParams {
    pub spend: groth16::Parameters<Bls12>,
    pub spend_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,

    pub output: groth16::Parameters<Bls12>,
    pub output_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
}

impl Default for SaplingParams {
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    fn default() -> Self {
        Self::new()
    }
}

impl SaplingParams {
    /// Download the Sapling Groth16 parameters if needed,
    /// check they were downloaded correctly, and load them into Zebra.
    ///
    /// # Panics
    ///
    /// If the downloaded or pre-existing parameter files are invalid.
    pub fn new() -> SaplingParams {
        let params_dir =
            zcash_proofs::default_params_folder().expect("unable to find user home directory");
        let failure_hint = format!(
            "Hint: try deleting {:?}, then running 'zebrad download' to re-download the parameters",
            params_dir
        );

        // TODO: Sprout

        let mut spend_path = params_dir.clone();
        spend_path.push("sapling-spend.params");

        let mut output_path = params_dir;
        output_path.push("sapling-output.params");

        // Download parameters if needed.
        //
        // TODO: use try_exists when it stabilises, to exit early on permissions errors (#83186)
        if !spend_path.exists() || !output_path.exists() {
            tracing::info!("downloading Zcash Sapling parameters");
            zcash_proofs::download_parameters()
                .unwrap_or_else(|_| panic!("error downloading parameter files. {}", failure_hint));
        }

        // TODO: if loading fails, log a message including `failure_hint`
        tracing::info!("checking and loading Zcash Sapling parameters");
        let parameters = zcash_proofs::load_parameters(&spend_path, &output_path, None);

        SaplingParams {
            spend: parameters.spend_params,
            spend_prepared_verifying_key: parameters.spend_vk,
            output: parameters.output_params,
            output_prepared_verifying_key: parameters.output_vk,
        }
    }
}
