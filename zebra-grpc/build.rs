//! Build script for zebra-grpc.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/service.proto")?;
    Ok(())
}
