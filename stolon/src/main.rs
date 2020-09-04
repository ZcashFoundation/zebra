use std::{net::SocketAddr, sync::Arc};

//use futures::stream::{FuturesUnordered, StreamExt};
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

    /*
    let tasks = FuturesUnordered::new();
    for _ in 0..opt.fanout {
        tasks.push(tokio::spawn(broadcast(tor_addr, network, transaction.clone())));
    }
    */

    // This can't actually be spawned, because
    // https://github.com/rust-lang/rust/issues/64552 causes rustc to "forget"
    // that the boxed error is actually 'static, which makes all of the
    // BoxError-returning functions impossible to use in a spawned task.
    // This also occurs when BoxError is replaced with eyre::Report.

    // Run fanout tasks sequentially to work around the 'static bug.
    for _ in 0..opt.fanout {
        let _ = send_transaction(tor_addr, network, transaction.clone()).await;
    }

    //tasks.collect::<Vec<_>>().await;
}
