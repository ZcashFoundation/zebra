//! Download the Sapling and Sprout Groth16 parameters if needed,
//! check they were downloaded correctly, and load them into Zebra.

// Has the same functionality as:
// <https://github.com/zcash/librustzcash/blob/c48bb4def2e122289843ddb3cb2984c325c03ca0/zcash_proofs/examples/download-params.rs>

fn main() {
    // The lazy static initializer does the download, if needed,
    // and the file hash checks.
    lazy_static::initialize(&zebra_consensus::groth16::GROTH16_PARAMETERS);
}
