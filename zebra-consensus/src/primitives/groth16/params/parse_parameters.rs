//! Copied from `zcash_proofs` v1.14.0 with minor modifications to the `parse_parameters` function (renamed to `parse_sapling_parameters`).
//!
//! Sapling related constants: <https://github.com/zcash/librustzcash/blob/zcash_proofs-0.14.0/zcash_proofs/src/lib.rs#L52-L58>
//! Parse parameters: <https://github.com/zcash/librustzcash/blob/zcash_proofs-0.14.0/zcash_proofs/src/lib.rs#L353>
//! Hash reader: <https://github.com/zcash/librustzcash/blob/zcash_proofs-0.14.0/zcash_proofs/src/hashreader.rs#L10>
//! Verify hash: <https://github.com/zcash/librustzcash/blob/zcash_proofs-0.14.0/zcash_proofs/src/lib.rs#L472>
use std::{
    fmt::Write,
    io::{self, Read},
};

use bellman::groth16;
use blake2b_simd::State;
use bls12_381::Bls12;
use zcash_proofs::{SAPLING_OUTPUT_NAME, SAPLING_SPEND_NAME};

use super::SaplingParameters;

// Circuit names

// Circuit hashes
const SAPLING_SPEND_HASH: &str = "8270785a1a0d0bc77196f000ee6d221c9c9894f55307bd9357c3f0105d31ca63991ab91324160d8f53e2bbd3c2633a6eb8bdf5205d822e7f3f73edac51b2b70c";
const SAPLING_OUTPUT_HASH: &str = "657e3d38dbb5cb5e7dd2970e8b03d69b4787dd907285b5a7f0790dcc8072f60bf593b32cc2d1c030e00ff5ae64bf84c5c3beb84ddc841d48264b4a171744d028";

// Circuit parameter file sizes
const SAPLING_SPEND_BYTES: u64 = 47958396;
const SAPLING_OUTPUT_BYTES: u64 = 3592860;

/// Parse Bls12 keys from bytes as serialized by [`groth16::Parameters::write`].
///
/// This function will panic if it encounters unparsable data.
///
/// [`groth16::Parameters::write`]: bellman::groth16::Parameters::write
pub fn parse_sapling_parameters<R: io::Read>(spend_fs: R, output_fs: R) -> SaplingParameters {
    let mut spend_fs = HashReader::new(spend_fs);
    let mut output_fs = HashReader::new(output_fs);

    // Deserialize params
    let spend_params = groth16::Parameters::<Bls12>::read(&mut spend_fs, false)
        .expect("couldn't deserialize Sapling spend parameters");
    let output_params = groth16::Parameters::<Bls12>::read(&mut output_fs, false)
        .expect("couldn't deserialize Sapling spend parameters");

    // There is extra stuff (the transcript) at the end of the parameter file which is
    // used to verify the parameter validity, but we're not interested in that. We do
    // want to read it, though, so that the BLAKE2b computed afterward is consistent
    // with `b2sum` on the files.
    let mut sink = io::sink();

    // TODO: use the correct paths for Windows and macOS
    //       use the actual file paths supplied by the caller
    verify_hash(
        spend_fs,
        &mut sink,
        SAPLING_SPEND_HASH,
        SAPLING_SPEND_BYTES,
        SAPLING_SPEND_NAME,
        "a file",
    )
    .expect(
        "Sapling spend parameter file is not correct, \
         please clean your `~/.zcash-params/` and re-run `fetch-params`.",
    );

    verify_hash(
        output_fs,
        &mut sink,
        SAPLING_OUTPUT_HASH,
        SAPLING_OUTPUT_BYTES,
        SAPLING_OUTPUT_NAME,
        "a file",
    )
    .expect(
        "Sapling output parameter file is not correct, \
         please clean your `~/.zcash-params/` and re-run `fetch-params`.",
    );

    // Prepare verifying keys
    let spend_vk = groth16::prepare_verifying_key(&spend_params.vk);
    let output_vk = groth16::prepare_verifying_key(&output_params.vk);

    SaplingParameters {
        spend: spend_params,
        spend_prepared_verifying_key: spend_vk,
        output: output_params,
        output_prepared_verifying_key: output_vk,
    }
}

/// Abstraction over a reader which hashes the data being read.
pub struct HashReader<R: Read> {
    reader: R,
    hasher: State,
    byte_count: u64,
}

impl<R: Read> HashReader<R> {
    /// Construct a new `HashReader` given an existing `reader` by value.
    pub fn new(reader: R) -> Self {
        HashReader {
            reader,
            hasher: State::new(),
            byte_count: 0,
        }
    }

    /// Destroy this reader and return the hash of what was read.
    pub fn into_hash(self) -> String {
        let hash = self.hasher.finalize();

        let mut s = String::new();
        for c in hash.as_bytes().iter() {
            write!(&mut s, "{c:02x}").expect("writing to a string never fails");
        }

        s
    }

    /// Return the number of bytes read so far.
    pub fn byte_count(&self) -> u64 {
        self.byte_count
    }
}

impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let bytes = self.reader.read(buf)?;

        if bytes > 0 {
            self.hasher.update(&buf[0..bytes]);
            let byte_count = u64::try_from(bytes).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Could not fit the number of read bytes into u64.",
                )
            })?;
            self.byte_count += byte_count;
        }

        Ok(bytes)
    }
}

/// Check if the Blake2b hash from `hash_reader` matches `expected_hash`,
/// while streaming from `hash_reader` into `sink`.
///
/// `hash_reader` can be used to partially read its inner reader's data,
/// before verifying the hash using this function.
///
/// Returns an error containing `name` and `params_source` on failure.
fn verify_hash<R: io::Read, W: io::Write>(
    mut hash_reader: HashReader<R>,
    mut sink: W,
    expected_hash: &str,
    expected_bytes: u64,
    name: &str,
    params_source: &str,
) -> Result<(), io::Error> {
    let read_result = io::copy(&mut hash_reader, &mut sink);

    if let Err(read_error) = read_result {
        return Err(io::Error::new(
            read_error.kind(),
            format!(
                "{} failed reading:\n\
                 expected: {} bytes,\n\
                 actual:   {} bytes from {:?},\n\
                 error: {:?}",
                name,
                expected_bytes,
                hash_reader.byte_count(),
                params_source,
                read_error,
            ),
        ));
    }

    let byte_count = hash_reader.byte_count();
    let hash = hash_reader.into_hash();
    if hash != expected_hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{name} failed validation:\n\
                 expected: {expected_hash} hashing {expected_bytes} bytes,\n\
                 actual:   {hash} hashing {byte_count} bytes from {params_source:?}",
            ),
        ));
    }

    Ok(())
}
