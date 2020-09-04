use std::{net::SocketAddr, sync::Arc};

use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;
use tower::{Service, ServiceExt};

pub use zebra_chain::{
    parameters::Network, serialization::ZcashDeserialize, transaction::Transaction,
};

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

// Tor's IsolateSOCKSAuth option (on by default) prevents sharing circuits
// for streams for which different SOCKS authentication was provided.

const SOCKS_USERNAME: &str = "stolon";
fn socks_password() -> String {
    use rand::distributions::{Alphanumeric, Distribution};
    Alphanumeric
        .sample_iter(rand::thread_rng())
        .take(16)
        .collect()
}

/// Send a transaction over Tor to the Zcash network, using DNS seeders to find peers.
#[tracing::instrument(skip(transaction))]
pub async fn send_transaction(
    tor_addr: SocketAddr,
    network: Network,
    transaction: Arc<Transaction>,
) -> Result<(), BoxError> {
    // We want to ensure that we get a different peer from the DNS seeder for
    // each call, so that fanout will send multiple times to different peers
    // rather than multiple times to the same peer. However, Tor only sends a
    // single IP address per RESOLVED cell, and caches DNS lookups. As a
    // workaround, instead of looking up an IP and then connecting, try
    // connecting directly to the DNS seeder, so that DNS resolution is
    // performed by the exit. Since we use a new circuit for each call, we hit a
    // different exit each time.
    let seed_domain = match network {
        Network::Mainnet => "mainnet.seeder.zfnd.org:8233",
        Network::Testnet => "testnet.seeder.zfnd.org:18233",
    };
    let conn = Socks5Stream::connect_with_password(
        tor_addr,
        seed_domain,
        SOCKS_USERNAME,
        socks_password().as_str(),
    )
    .await?
    .into_inner();

    send_transaction_inner(conn, transaction).await
}

/// Send a transaction over Tor to a specific peer.
#[tracing::instrument(skip(transaction))]
pub async fn send_transaction_to(
    tor_addr: SocketAddr,
    peer_addr: SocketAddr,
    transaction: Arc<Transaction>,
) -> Result<(), BoxError> {
    let conn = Socks5Stream::connect_with_password(
        tor_addr,
        peer_addr,
        SOCKS_USERNAME,
        socks_password().as_str(),
    )
    .await?
    .into_inner();

    send_transaction_inner(conn, transaction).await
}

async fn send_transaction_inner(
    conn: TcpStream,
    transaction: Arc<Transaction>,
) -> Result<(), BoxError> {
    let client = zebra_network::connect_isolated(conn, "".to_string()).await?;

    tracing::debug!(hash = ?transaction.hash(), "sending transaction");
    client
        .ready_oneshot()
        .await?
        .call(zebra_network::Request::PushTransaction(transaction))
        .await?;

    Ok(())
}
