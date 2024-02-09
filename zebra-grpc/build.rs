//! Compile proto files

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .btree_map(["."])
        .compile(&["proto/scanner.proto"], &[""])?;
    Ok(())
}
