//! Compile proto files
use std::{env, fs, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR").map(PathBuf::from);
    tonic_prost_build::configure()
        .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
        .file_descriptor_set_path(out_dir.unwrap().join("indexer_descriptor.bin"))
        .compile_protos(&["proto/indexer.proto"], &[""])?;

    if env::var_os("ZALLET").is_some() {
        // The following code will clone the zallet repo and build the binary,
        // then copy the binary to the project target directory.
        //
        // Code below is fragile and will just build the main branch of the wallet repository
        // so we can have it available for `qa` regtests.

        let build_dir = env::var("OUT_DIR").map(PathBuf::from).unwrap_or_default();

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

    Ok(())
}
