//! Compile proto files
use std::{env, fs, path::PathBuf, process::Command};

const ZALLET_COMMIT: Option<&str> = None;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_or_copy_proto()?;
    build_zallet_for_qa_tests();

    Ok(())
}

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

fn build_zallet_for_qa_tests() {
    if env::var_os("ZALLET").is_some() {
        // The following code will clone the zallet repo and build the binary,
        // then copy the binary to the project target directory.
        //
        // Code below is fragile and will just build the main branch of the wallet repository
        // so we can have it available for `qa` regtests.

        let build_dir = env::var("OUT_DIR")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("zallet_build");

        let profile = "debug".to_string();

        let target_dir = env::var("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().expect("failed to get current dir"))
            .join("../target")
            .join(&profile);

        let _ = Command::new("git")
            .args([
                "clone",
                "https://github.com/zcash/wallet.git",
                build_dir.to_str().unwrap(),
            ])
            .status()
            .expect("failed to clone external binary");

        if let Some(zallet_commit) = ZALLET_COMMIT {
            let _ = Command::new("git")
                .args(["checkout", zallet_commit])
                .current_dir(&build_dir)
                .status()
                .expect("failed to build external binary");
        }

        let _ = Command::new("cargo")
            .args(["build"])
            .current_dir(&build_dir)
            .status()
            .expect("failed to build external binary");

        fs::copy(
            build_dir.join(format!("target/{profile}/zallet")),
            target_dir.join("zallet"),
        )
        .unwrap_or_else(|_| {
            panic!(
                "failed to copy zallet binary from {} to {}",
                build_dir.display(),
                target_dir.display()
            )
        });
    }
}
