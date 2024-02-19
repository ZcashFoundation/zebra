//! Compile proto files

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .btree_map(["."])
        .protoc_arg("--experimental_allow_proto3_optional")
        .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
        .compile(&["proto/scanner.proto"], &[""])?;
    Ok(())
}
