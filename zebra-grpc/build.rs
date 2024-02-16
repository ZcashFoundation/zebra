//! Compile proto files

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .btree_map(["."])
        .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
        .compile(&["proto/scanner.proto"], &[""])?;
    Ok(())
}
