//! Compile proto files

use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    tonic_build::configure()
        .btree_map(["."])
        .protoc_arg("--experimental_allow_proto3_optional")
        .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
        .file_descriptor_set_path(out_dir.join("scanner_descriptor.bin"))
        .compile_protos(&["proto/scanner.proto"], &[""])?;

    Ok(())
}
