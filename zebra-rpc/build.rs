//! Compile proto files
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "indexer-rpcs")]
    build_or_copy_proto()?;

    Ok(())
}

#[cfg(feature = "indexer-rpcs")]
fn build_or_copy_proto() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR")
        .map(PathBuf::from)
        .expect("requires OUT_DIR environment variable definition");
    let file_names = ["indexer_descriptor.bin", "zebra.indexer.rpc.rs"];

    let is_protoc_available = env::var_os("PROTOC")
        .map(PathBuf::from)
        .or_else(|| which::which("protoc").ok())
        .is_some();

    if is_protoc_available {
        tonic_build::configure()
            .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
            .file_descriptor_set_path(out_dir.join("indexer_descriptor.bin"))
            .compile_protos(&["proto/indexer.proto"], &[""])?;

        for file_name in file_names {
            fs::copy(
                out_dir.join(file_name),
                format!("proto/__generated__/{file_name}"),
            )?;
        }
    } else {
        for file_name in file_names {
            fs::copy(
                format!("proto/__generated__/{file_name}"),
                out_dir.join(file_name),
            )?;
        }
    }

    Ok(())
}
