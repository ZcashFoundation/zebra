//! Compile proto files

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "indexer-rpcs")]
    {
        let out_dir = std::env::var("OUT_DIR").map(std::path::PathBuf::from);
        tonic_build::configure()
            .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
            .file_descriptor_set_path(out_dir.unwrap().join("indexer_descriptor.bin"))
            .compile_protos(&["proto/indexer.proto"], &[""])?;
    }

    if std::env::var_os("ZALLET").is_some() {
        use std::{env, fs, path::PathBuf, process::Command};

        // The following code will clone the zallet repo and build the binary,
        // then copy the binary to the project target directory.
        //
        // Code below is fragile and will just build the main branch of the wallet repository
        // so we can have it available for `qa` regtests.

        let build_dir = env::var("OUT_DIR")
            .map(PathBuf::from)
            .expect("failed to get OUT_DIR");

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
            build_dir.join(format!("target/{}/zallet", profile)),
            target_dir.join("zallet"),
        )
        .expect("failed to copy zallet binary");
    }

    Ok(())
}
