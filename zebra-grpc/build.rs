//! Compile proto files

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/scanner.proto")?;
    Ok(())
}
