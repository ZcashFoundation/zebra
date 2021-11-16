use bellman::groth16;
use bls12_381::Bls12;

lazy_static::lazy_static! {
    pub static ref PARAMS: Groth16Params = Groth16Params::new();
}

#[non_exhaustive]
pub struct Groth16Params {
    pub sapling: SaplingParams,
}

impl Groth16Params {
    fn new() -> Self {
        Self {
            sapling: SaplingParams::new(),
        }
    }
}

#[non_exhaustive]
pub struct SaplingParams {
    pub spend: groth16::Parameters<Bls12>,
    pub spend_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,

    pub output: groth16::Parameters<Bls12>,
    pub output_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
}

impl SaplingParams {
    fn new() -> SaplingParams {
        let params_dir =
            zcash_proofs::default_params_folder().expect("unable to find user home directory");

        // TODO: Sprout

        let mut spend_path = params_dir.clone();
        spend_path.push("sapling-spend.params");

        let mut output_path = params_dir;
        output_path.push("sapling-output.params");

        // Download parameters if needed.
        //
        // TODO: use try_exists when it stabilises, to exit early on permissions errors (#83186)
        if !spend_path.exists() || !output_path.exists() {
            zcash_proofs::download_parameters().expect("error downloading parameter files");
        }

        let parameters = zcash_proofs::load_parameters(&spend_path, &output_path, None);

        SaplingParams {
            spend: parameters.spend_params,
            spend_prepared_verifying_key: parameters.spend_vk,
            output: parameters.output_params,
            output_prepared_verifying_key: parameters.output_vk,
        }
    }
}
