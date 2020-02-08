//! `connect` subcommand - test stub for talking to zcashd

use crate::{
    error::{Error, ErrorKind},
    prelude::*,
};

use abscissa_core::{Command, Options, Runnable};

use futures::prelude::*;

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

        use crate::components::tokio::TokioComponent;
        let rt = app_writer()
            .state_mut()
            .components
            .get_downcast_mut::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .take();

        rt.expect("runtime should not already be taken")
            .block_on(self.connect())
            // Surface any error that occurred executing the future.
            .unwrap();
    }
}

impl ConnectCmd {
    async fn connect(&self) -> Result<(), Error> {
        use zebra_network::{Request, Response};

        info!("begin tower-based peer handling test stub");
        use tower::{buffer::Buffer, service_fn, Service, ServiceExt};

        let node = Buffer::new(
            service_fn(|req| async move {
                info!(?req);
                Ok::<Response, Error>(Response::Nil)
            }),
            1,
        );

        let mut config = app_config().network.clone();
        // Use a different listen addr so that we don't conflict with another local node.
        config.listen_addr = "127.0.0.1:38233".parse().unwrap();
        // Connect only to the specified peer.
        config.initial_mainnet_peers = vec![self.addr.to_string()];

        let (mut peer_set, _address_book) = zebra_network::init(config, node).await;

        info!("waiting for peer_set ready");
        peer_set
            .ready()
            .await
            .map_err(|e| Error::from(ErrorKind::Io.context(e)))?;

        info!("peer_set became ready");

        peer_set.ready().await.unwrap();

        use zebra_chain::block::BlockHeaderHash;
        use zebra_chain::serialization::ZcashDeserialize;
        let hash_415000 = BlockHeaderHash::zcash_deserialize(
            &[
                104, 97, 133, 175, 186, 67, 219, 26, 10, 37, 145, 232, 63, 170, 25, 37, 8, 250, 47,
                43, 38, 113, 231, 60, 121, 55, 171, 1, 0, 0, 0, 0,
            ][..],
        )
        .unwrap();
        let rsp = peer_set
            .call(Request::BlocksByHash(
                std::iter::once(hash_415000).collect(),
            ))
            .await;

        info!(?rsp);

        let block_415000 = if let Ok(Response::Blocks(blocks)) = rsp {
            blocks[0].clone()
        } else {
            panic!("did not get block");
        };

        let hash_414999 = block_415000.header.previous_block_hash;

        let two_blocks =
            Request::BlocksByHash([hash_415000, hash_414999].iter().cloned().collect());
        info!(?two_blocks);
        peer_set.ready().await.unwrap();
        let mut rsp = peer_set.call(two_blocks.clone()).await;
        info!(?rsp);
        while let Err(_) = rsp {
            info!("retry");
            peer_set.ready().await.unwrap();
            rsp = peer_set.call(two_blocks.clone()).await;
            info!(?rsp);
        }

        let eternity = future::pending::<()>();
        eternity.await;

        Ok(())
    }
}
