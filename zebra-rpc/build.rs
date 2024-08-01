//! Compile proto files

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "indexer-rpcs")]
    {
        use std::{env, path::PathBuf};
        let out_dir = env::var("OUT_DIR").map(PathBuf::from);

        build_json_codec_service();

        tonic_build::configure()
            .type_attribute(".", "#[derive(serde::Deserialize, serde::Serialize)]")
            .file_descriptor_set_path(out_dir.unwrap().join("indexer_descriptor.bin"))
            .compile(&["proto/indexer.proto"], &[""])?;
    }

    Ok(())
}

fn build_json_codec_service() {
    let indexer_service = tonic_build::manual::Service::builder()
        .name("Indexer")
        .package("json.indexer")
        .method(
            tonic_build::manual::Method::builder()
                .name("chain_tip_change")
                .route_name("ChainTipChange")
                .input_type("crate::indexer::types::Empty")
                .output_type("crate::indexer::types::Empty")
                .codec_path("crate::indexer::codec::JsonCodec")
                .server_streaming()
                .build(),
        )
        .build();

    tonic_build::manual::Builder::new().compile(&[indexer_service]);
}
