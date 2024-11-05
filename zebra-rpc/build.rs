//! Compile proto files

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, path::PathBuf};
    let out_dir = env::var("OUT_DIR").map(PathBuf::from);

    tonic_build::configure()
        .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
        .file_descriptor_set_path(out_dir.unwrap().join("endpoint_descriptor.bin"))
        .compile_protos(&["proto/endpoint.proto"], &[""])?;

    #[cfg(feature = "indexer-rpcs")]
    {
        let out_dir = env::var("OUT_DIR").map(PathBuf::from);
        tonic_build::configure()
            .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
            .file_descriptor_set_path(out_dir.unwrap().join("indexer_descriptor.bin"))
            .compile_protos(&["proto/indexer.proto"], &[""])?;
    }

    Ok(())
}
