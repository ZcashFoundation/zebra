use std::io::{self, BufReader};

use bellman::groth16;
use bls12_381::Bls12;

use super::HashReader;

const SAPLING_SPEND_HASH: &str = "8270785a1a0d0bc77196f000ee6d221c9c9894f55307bd9357c3f0105d31ca63991ab91324160d8f53e2bbd3c2633a6eb8bdf5205d822e7f3f73edac51b2b70c";
const SAPLING_OUTPUT_HASH: &str = "657e3d38dbb5cb5e7dd2970e8b03d69b4787dd907285b5a7f0790dcc8072f60bf593b32cc2d1c030e00ff5ae64bf84c5c3beb84ddc841d48264b4a171744d028";
const SPROUT_HASH: &str = "e9b238411bd6c0ec4791e9d04245ec350c9c5744f5610dfcce4365d5ca49dfefd5054e371842b3f88fa1b9d7e8e075249b3ebabd167fa8b0f3161292d36c180a";

lazy_static::lazy_static! {
    pub static ref PARAMS: Groth16Params = Groth16Params::new();
}

#[non_exhaustive]
pub struct Groth16Params {
    pub sprout: SproutParams,
    pub sapling: SaplingParams,
}

impl Groth16Params {
    fn new() -> Self {
        Self {
            sprout: SproutParams::new(),
            sapling: SaplingParams::new(),
        }
    }
}

#[non_exhaustive]
pub struct SproutParams {
    pub verifying_key: groth16::VerifyingKey<Bls12>,
    pub prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
}

impl SproutParams {
    fn new() -> Self {
        let sprout_fs = SproutParams::load();
        let sprout_fs = BufReader::with_capacity(1024 * 1024, &sprout_fs[..]);
        Self::read(sprout_fs).expect("reading parameters from vec will always succeed")
    }

    fn load() -> Vec<u8> {
        todo!("figure out how we want to actually load these, file, download, include, etc");
    }

    fn read<R: io::Read>(fs: R) -> Result<Self, io::Error> {
        let mut fs = HashReader::new(fs);
        let verifying_key = groth16::VerifyingKey::<Bls12>::read(&mut fs)?;

        // There is extra stuff (the transcript) at the end of the parameter file which is
        // used to verify the parameter validity, but we're not interested in that. We do
        // want to read it, though, so that the BLAKE2b computed afterward is consistent
        // with `b2sum` on the files.
        io::copy(&mut fs, &mut io::sink())?;

        if fs.into_hash() != SPROUT_HASH {
            panic!("Sprout groth16 parameter is not correct.");
        }

        let prepared_verifying_key = groth16::prepare_verifying_key(&verifying_key);

        Ok(Self {
            verifying_key,
            prepared_verifying_key,
        })
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
    fn new() -> Self {
        let (spend, output) = wagyu_zcash_parameters::load_sapling_parameters();
        let spend_fs = BufReader::with_capacity(1024 * 1024, &spend[..]);
        let output_fs = BufReader::with_capacity(1024 * 1024, &output[..]);

        Self::read(spend_fs, output_fs)
            .expect("reading parameters from wagyu zcash parameter's vec will always succeed")
    }

    fn read<R: io::Read>(spend_fs: R, output_fs: R) -> Result<Self, io::Error> {
        let mut spend_fs = HashReader::new(spend_fs);
        let mut output_fs = HashReader::new(output_fs);

        // Deserialize params
        let spend = groth16::Parameters::<Bls12>::read(&mut spend_fs, false)?;
        let output = groth16::Parameters::<Bls12>::read(&mut output_fs, false)?;

        // There is extra stuff (the transcript) at the end of the parameter file which is
        // used to verify the parameter validity, but we're not interested in that. We do
        // want to read it, though, so that the BLAKE2b computed afterward is consistent
        // with `b2sum` on the files.
        let mut sink = io::sink();
        io::copy(&mut spend_fs, &mut sink)?;
        io::copy(&mut output_fs, &mut sink)?;

        if spend_fs.into_hash() != SAPLING_SPEND_HASH {
            panic!("Sapling spend parameter is not correct.");
        }

        if output_fs.into_hash() != SAPLING_OUTPUT_HASH {
            panic!("Sapling output parameter is not correct.");
        }

        // Prepare verifying keys
        let spend_prepared_verifying_key = groth16::prepare_verifying_key(&spend.vk);
        let output_prepared_verifying_key = groth16::prepare_verifying_key(&output.vk);

        Ok(Self {
            spend,
            spend_prepared_verifying_key,
            output,
            output_prepared_verifying_key,
        })
    }
}
