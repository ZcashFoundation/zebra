//! `connect` subcommand - test stub for talking to zcashd

use crate::{components::tokio::TokioComponent, prelude::*};
use abscissa_core::{Command, Options, Runnable};
use color_eyre::Report;
use eyre::{eyre, WrapErr};
use futures::{
    prelude::*,
    stream::{FuturesUnordered, StreamExt},
};
use std::collections::BTreeSet;
use tower::{buffer::Buffer, service_fn, Service, ServiceExt};
use zebra_chain::{block::BlockHeaderHash, types::BlockHeight};

// genesis
static GENESIS: BlockHeaderHash = BlockHeaderHash([
    8, 206, 61, 151, 49, 176, 0, 192, 131, 56, 69, 92, 138, 74, 107, 208, 93, 161, 110, 38, 177,
    29, 170, 27, 145, 113, 132, 236, 232, 15, 4, 0,
]);

/// `connect` subcommand
#[derive(Command, Debug, Options)]
pub struct ConnectCmd {
    /// The address of the node to connect to.
    #[options(
        help = "The address of the node to connect to.",
        default = "127.0.0.1:8233"
    )]
    addr: std::net::SocketAddr,
}

impl Runnable for ConnectCmd {
    /// Start the application.
    fn run(&self) {
        info!(connect.addr = ?self.addr);

        let rt = app_writer()
            .state_mut()
            .components
            .get_downcast_mut::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .take();

        let result = rt
            .expect("runtime should not already be taken")
            .block_on(self.connect());

        match result {
            Ok(()) => {}
            Err(e) => {
                eprintln!("Error: {:?}", e);
                std::process::exit(1);
            }
        }
    }
}

impl ConnectCmd {
    async fn connect(&self) -> Result<(), Report> {
        info!(?self, "begin tower-based peer handling test stub");

        // The service that our node uses to respond to requests by peers
        let node = Buffer::new(
            service_fn(|req| async move {
                info!(?req);
                Ok::<zebra_network::Response, Report>(zebra_network::Response::Nil)
            }),
            1,
        );

        let mut config = app_config().network.clone();
        // Use a different listen addr so that we don't conflict with another local node.
        config.listen_addr = "127.0.0.1:38233".parse()?;
        // Connect only to the specified peer.
        config.initial_mainnet_peers.insert(self.addr.to_string());

        let state = zebra_state::in_memory::init();
        let (peer_set, _address_book) = zebra_network::init(config, node).await;
        let retry_peer_set = tower::retry::Retry::new(zebra_network::RetryErrors, peer_set.clone());

        let mut downloaded_block_heights = BTreeSet::<BlockHeight>::new();
        downloaded_block_heights.insert(BlockHeight(0));

        let mut connect = Connect {
            retry_peer_set,
            peer_set,
            state,
            tip: GENESIS,
            block_requests: FuturesUnordered::new(),
            requested_block_heights: 0,
            downloaded_block_heights,
        };

        connect.connect().await
    }
}

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

struct Connect<ZN, ZS>
where
    ZN: Service<zebra_network::Request>,
{
    retry_peer_set: tower::retry::Retry<zebra_network::RetryErrors, ZN>,
    peer_set: ZN,
    state: ZS,
    tip: BlockHeaderHash,
    block_requests: FuturesUnordered<ZN::Future>,
    requested_block_heights: usize,
    downloaded_block_heights: BTreeSet<BlockHeight>,
}

impl<ZN, ZS> Connect<ZN, ZS>
where
    ZN: Service<zebra_network::Request, Response = zebra_network::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    ZN::Future: Send,
    ZS: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    ZS::Future: Send,
{
    async fn connect(&mut self) -> Result<(), Report> {
        // TODO(jlusby): Replace with real state service

        while self.requested_block_heights < 700_000 {
            let hashes = self.next_hashes().await?;
            self.tip = *hashes.last().unwrap();

            // Request the corresponding blocks in chunks
            self.request_blocks(hashes).await?;

            // Allow at most 300 block requests in flight.
            self.drain_requests(300).await?;
        }

        self.drain_requests(0).await?;

        let eternity = future::pending::<()>();
        eternity.await;

        Ok(())
    }

    async fn next_hashes(&mut self) -> Result<Vec<BlockHeaderHash>, Report> {
        // Request the next 500 hashes.
        self.retry_peer_set
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_network::Request::FindBlocks {
                known_blocks: vec![self.tip],
                stop: None,
            })
            .await
            .map_err(|e| eyre!(e))
            .wrap_err("request failed, TODO implement retry")
            .map(|response| match response {
                zebra_network::Response::BlockHeaderHashes(hashes) => hashes,
                _ => unreachable!("FindBlocks always gets a BlockHeaderHashes response"),
            })
            .map(|hashes| {
                info!(
                    new_hashes = hashes.len(),
                    requested = self.requested_block_heights,
                    in_flight = self.block_requests.len(),
                    downloaded = self.downloaded_block_heights.len(),
                    highest = self.downloaded_block_heights.iter().next_back().unwrap().0,
                    "requested more hashes"
                );
                self.requested_block_heights += hashes.len();
                hashes
            })
    }

    async fn request_blocks(&mut self, hashes: Vec<BlockHeaderHash>) -> Result<(), Report> {
        for chunk in hashes.chunks(10usize) {
            let request = self.peer_set.ready_and().await.map_err(|e| eyre!(e))?.call(
                zebra_network::Request::BlocksByHash(chunk.iter().cloned().collect()),
            );

            self.block_requests.push(request);
        }

        Ok(())
    }

    async fn drain_requests(&mut self, request_goal: usize) -> Result<(), Report> {
        while self.block_requests.len() > request_goal {
            match self.block_requests.next().await {
                Some(Ok(zebra_network::Response::Blocks(blocks))) => {
                    for block in blocks {
                        self.downloaded_block_heights
                            .insert(block.coinbase_height().unwrap());
                        self.state
                            .ready_and()
                            .await
                            .map_err(|e| eyre!(e))?
                            .call(zebra_state::Request::AddBlock { block })
                            .await
                            .map_err(|e| eyre!(e))?;
                    }
                }
                Some(Err(e)) => {
                    error!(%e);
                }
                _ => continue,
            }
        }

        Ok(())
    }
}
