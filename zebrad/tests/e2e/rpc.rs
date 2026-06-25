use color_eyre::eyre::Result;

/// Test successful `getpeerinfo` RPC call against public Mainnet peers.
///
/// See [`crate::common::get_block_template_rpcs::get_peer_info`] for more information.
#[tokio::test]
#[ignore]
async fn get_peer_info() -> Result<()> {
    crate::common::get_block_template_rpcs::get_peer_info::run().await
}
