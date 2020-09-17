use std::{net::SocketAddr, sync::Arc};

use futures::stream::{FuturesUnordered, StreamExt};
use structopt::StructOpt;

use stolon::*;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "stolon",
    about = "Reads a Zcash transaction from stdin and broadcasts it to the network over Tor."
)]
struct Opt {
    /// The address of the Tor SOCKS proxy to use for diffusion.
    #[structopt(short, long)]
    proxy_addr: SocketAddr,
    /// Sends the transaction to testnet instead of mainnet.
    #[structopt(short, long)]
    testnet: bool,
    /// Perform this many broadcasts simultaneously.
    #[structopt(short, long, default_value = "3")]
    fanout: usize,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let opt = Opt::from_args();
    tracing::debug!(?opt);

    let tor_addr = opt.proxy_addr;
    let network = if opt.testnet {
        Network::Testnet
    } else {
        Network::Mainnet
    };

    let transaction = Arc::<Transaction>::zcash_deserialize(std::io::stdin())
        .expect("could not read transaction from stdin");

    let tasks = FuturesUnordered::new();
    for _ in 0..opt.fanout {
        tasks.push(tokio::spawn(send_transaction(
            tor_addr,
            network,
            transaction.clone(),
        )));
    }

    let rsp = tasks
        .map(|task| task.expect("spawned tasks should not panic"))
        .collect::<Vec<_>>()
        .await;
    tracing::debug!(?rsp);

    let ok_count = rsp.iter().filter(|r| r.is_ok()).count();
    let err_count = rsp.iter().filter(|r| r.is_err()).count();
    tracing::info!(opt.fanout, ok_count, err_count);

    // If this binary is used as a component of some other program, rather than
    // as a standalone tool, it would be useful to use error codes to signal
    // success or failure. But at the moment, there are no such concrete users,
    // so let's wait until there are before polishing the tool.
    if ok_count == 0 {
        tracing::error!("failed to send transaction to any peers")
    }
}
