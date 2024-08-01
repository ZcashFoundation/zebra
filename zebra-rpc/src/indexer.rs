//! A tonic RPC server for Zebra's indexer API.

#[cfg(test)]
mod tests;

pub mod codec;
pub mod methods;
pub mod server;
pub mod types;

// The generated indexer proto
tonic::include_proto!("zebra.indexer.rpc");

pub(crate) const FILE_DESCRIPTOR_SET: &[u8] =
    tonic::include_file_descriptor_set!("indexer_descriptor");

pub mod json {
    include!(concat!(env!("OUT_DIR"), "/json.indexer.Indexer.rs"));
}

use std::pin::Pin;

use futures::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Response, Status};

struct Indexer;

// TODO: Figure out how to base which service a request is routed to by checking its content-type.
//       See https://github.com/hyperium/tonic/issues/851
//
// Or just set the codec based on a feature flag, comment on tonic#851, and open an issue for picking
// a codec based on the Content-Type header later (behind another feature flag?).
// Implement `From` for all of the types defined in Rust for all of the generated protobuf types?

#[tonic::async_trait]
impl json::indexer_server::Indexer for Indexer {
    type ChainTipChangeStream = Pin<Box<dyn Stream<Item = Result<types::Empty, Status>> + Send>>;

    async fn chain_tip_change(
        &self,
        _: tonic::Request<types::Empty>,
    ) -> Result<Response<Self::ChainTipChangeStream>, Status> {
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(133);
        let response_stream = ReceiverStream::new(response_receiver);

        tokio::spawn(async move {
            // Notify the client of chain tip changes until the channel is closed
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let tx = response_sender.clone();
                tokio::spawn(async move { tx.send(Ok(types::Empty {})).await });
            }

            let _ = response_sender
                .send(Err(Status::unavailable(
                    "chain_tip_change channel has closed",
                )))
                .await;
        });

        Ok(Response::new(Box::pin(response_stream)))
    }
}
