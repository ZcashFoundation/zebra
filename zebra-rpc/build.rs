//! Compile proto files
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Each step declares the files it reads. Without any `rerun-if-changed`, Cargo
    // reruns this script when the newest mtime in the package changes at all, even
    // when a copy such as a Docker image layer only truncates it to whole seconds.
    println!("cargo:rerun-if-env-changed=PROTOC");

    build_or_copy_proto()?;
    build_rpc_schema()?;

    Ok(())
}

fn build_or_copy_proto() -> Result<(), Box<dyn std::error::Error>> {
    // Zebra's indexer API.
    build_or_copy_proto_set(
        &["proto/indexer.proto"],
        &[""],
        "indexer_descriptor.bin",
        &["indexer_descriptor.bin", "zebra.indexer.rpc.rs"],
        true,
    )?;

    // The lightwalletd `CompactTxStreamer` client API.
    //
    // `service.proto` imports `compact_formats.proto`, so both are compiled with
    // the `proto` directory as the include path.
    build_or_copy_proto_set(
        &["proto/service.proto", "proto/compact_formats.proto"],
        &["proto"],
        "lightwalletd_descriptor.bin",
        &["lightwalletd_descriptor.bin", "cash.z.wallet.sdk.rpc.rs"],
        false,
    )?;

    Ok(())
}

/// Compiles the given proto files if `protoc` and the proto files are available,
/// copying the generated files into `proto/__generated__/` so they can be checked in.
/// Otherwise, copies the checked-in generated files into `OUT_DIR`.
fn build_or_copy_proto_set(
    proto_files: &[&str],
    include_dirs: &[&str],
    descriptor_name: &str,
    file_names: &[&str],
    serde_derives: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR").map(PathBuf::from)?;

    let are_proto_files_available = proto_files.iter().all(|path| Path::new(path).exists());
    let is_protoc_available = env::var_os("PROTOC")
        .map(PathBuf::from)
        .or_else(|| which::which("protoc").ok())
        .is_some();

    if are_proto_files_available && is_protoc_available {
        for proto_file in proto_files {
            println!("cargo:rerun-if-changed={proto_file}");
        }

        let mut config =
            tonic_prost_build::configure().file_descriptor_set_path(out_dir.join(descriptor_name));

        if serde_derives {
            config = config.type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]");
        }

        config.compile_protos(proto_files, include_dirs)?;

        for file_name in file_names {
            let out_path = out_dir.join(file_name);
            let generated_path = format!("proto/__generated__/{file_name}");
            if fs::read(&out_path).ok() != fs::read(&generated_path).ok() {
                fs::copy(out_path, generated_path)?;
            }
        }
    } else {
        for file_name in file_names {
            let out_path = out_dir.join(file_name);
            let generated_path = format!("proto/__generated__/{file_name}");
            println!("cargo:rerun-if-changed={generated_path}");
            if fs::read(&out_path).ok() != Some(fs::read(&generated_path)?) {
                fs::copy(generated_path, out_path)?;
            }
        }
    }

    Ok(())
}

fn build_rpc_schema() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR").map(PathBuf::from)?;
    let json_rpc_methods_rs = "src/methods.rs";
    println!("cargo:rerun-if-changed={json_rpc_methods_rs}");
    let trait_names = ["Rpc"];
    openrpsee::generate_openrpc(json_rpc_methods_rs, &trait_names, false, &out_dir)?;

    Ok(())
}
