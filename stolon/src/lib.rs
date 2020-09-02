use std::{net::SocketAddr, sync::Arc};

use tokio_socks::{tcp::Socks5Stream, TargetAddr};
use tower::{Service, ServiceExt};

pub use zebra_chain::{
    parameters::Network, serialization::ZcashDeserialize, transaction::Transaction,
};

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub async fn obtain_peer_addr(
    tor_addr: SocketAddr,
    network: Network,
) -> Result<SocketAddr, BoxError> {
    let seed_domain = match network {
        Network::Mainnet => "mainnet.seeder.zfnd.org:8233",
        Network::Testnet => "testnet.seeder.zfnd.org:18233",
    };

    tracing::debug!(?seed_domain, "using tor resolver to find peers");
    // this API is pretty awkward, let's upstream some patches.
    let mut peer_addr = match Socks5Stream::tor_resolve(tor_addr, seed_domain).await? {
        TargetAddr::Ip(addr) => addr,
        TargetAddr::Domain { .. } => return Err("resolving a domain gave a domain".into()),
    };

    peer_addr.set_port(match network {
        Network::Mainnet => 8233,
        Network::Testnet => 18233,
    });

    Ok(peer_addr)
}

pub fn choose_peer_addr(address_book: &zebra_network::AddressBook) -> Option<SocketAddr> {
    use rand::seq::SliceRandom;
    use zebra_network::types::PeerServices;

    let peers = address_book
        .potentially_connected_peers()
        .filter(|peer| peer.services.contains(PeerServices::NODE_NETWORK))
        .collect::<Vec<_>>();

    peers.choose(&mut rand::thread_rng()).map(|peer| peer.addr)
}

#[tracing::instrument(skip(transaction))]
pub async fn send_transaction(
    tor_addr: SocketAddr,
    peer_addr: SocketAddr,
    transaction: Arc<Transaction>,
) -> Result<(), BoxError> {
    use rand::distributions::{Alphanumeric, Distribution};

    // Tor's IsolateSOCKSAuth option (on by default) prevents sharing circuits
    // for streams for which different SOCKS authentication was provided.
    let socks_user = "stolon";
    let socks_pass = Alphanumeric
        .sample_iter(rand::thread_rng())
        .take(16)
        .collect::<String>();

    let conn =
        Socks5Stream::connect_with_password(tor_addr, peer_addr, socks_user, socks_pass.as_str())
            .await?
            .into_inner();

    let client = zebra_network::connect_isolated(conn, "".to_string()).await?;

    tracing::debug!(hash = ?transaction.hash(), "sending transaction");
    client
        .ready_oneshot()
        .await?
        .call(zebra_network::Request::PushTransaction(transaction))
        .await?;

    Ok(())
}
