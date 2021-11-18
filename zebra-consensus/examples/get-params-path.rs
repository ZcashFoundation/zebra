//! Print the Zcash parameter directory path to standard output.

// Modified from:
// https://github.com/zcash/librustzcash/blob/c48bb4def2e122289843ddb3cb2984c325c03ca0/zcash_proofs/examples/get-params-path.rs

fn main() {
    let path = zebra_consensus::groth16::Groth16Params::params_folder();
    if let Some(path) = path.to_str() {
        println!("{}", path);
    }
}
