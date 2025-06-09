//! Compile proto files
use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR").map(PathBuf::from);
    tonic_build::configure()
        .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
        .file_descriptor_set_path(out_dir.unwrap().join("indexer_descriptor.bin"))
        .compile_protos(&["proto/indexer.proto"], &[""])?;

    Ok(())
}
