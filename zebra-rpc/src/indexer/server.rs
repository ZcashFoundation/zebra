//! A tonic RPC server for Zebra's indexer API.

use std::{net::SocketAddr, time::Duration};

use tokio::task::JoinHandle;
use tonic::transport::{server::TcpIncoming, Server};
use tower::BoxError;
use zebra_chain::chain_tip::ChainTip;
use zebra_node_services::mempool::MempoolTxSubscriber;
use zebra_state::ReadState;

use crate::{indexer::indexer_server::IndexerServer, server::OPENED_RPC_ENDPOINT_MSG};

/// Maximum concurrent HTTP/2 streams per connection.
///
/// Matches hyper's HTTP/2 default so single-connection indexers can pipeline
/// concurrent `GetBlock` requests alongside long-lived change streams.
/// Paired with [`MAX_DECODING_MESSAGE_SIZE`], worst-case per-connection request
/// receive memory is bounded at `200 * 16 KiB = 3.125 MiB` (down from
/// `20 * 4 MiB = 80 MiB` under tonic's default 4 MiB decode cap).
pub(super) const MAX_CONCURRENT_STREAMS: u32 = 200;

/// Maximum encoded size of an incoming indexer or reflection gRPC request, in bytes.
///
/// The largest valid indexer request is `NonFinalizedStateChangeRequest` with
/// [`zebra_state::MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS`] (100) 32-byte
/// hashes, which encodes to 3,400 bytes (`100 * (1 tag + 1 length + 32 payload)`).
/// A 16 KiB cap leaves headroom for protobuf framing and reflection queries while
/// bounding pre-handler decode buffers and repeated-field expansion.
pub(super) const MAX_DECODING_MESSAGE_SIZE: usize = 16 * 1024;

/// Interval between HTTP/2 keepalive pings sent to detect dead connections.
const HTTP2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Timeout for HTTP/2 keepalive pings before the connection is closed.
const HTTP2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

type ServerTask = JoinHandle<Result<(), BoxError>>;

/// Indexer RPC service.
pub struct IndexerRPC<ReadStateService, Tip>
where
    ReadStateService: ReadState,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    pub(super) read_state: ReadStateService,
    pub(super) chain_tip_change: Tip,
    pub(super) mempool_change: MempoolTxSubscriber,
}

/// Initializes the indexer RPC server
#[tracing::instrument(skip_all)]
pub async fn init<ReadStateService, Tip>(
    listen_addr: SocketAddr,
    read_state: ReadStateService,
    chain_tip_change: Tip,
    mempool_change: MempoolTxSubscriber,
) -> Result<(ServerTask, SocketAddr), BoxError>
where
    ReadStateService: ReadState,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    let indexer_service = IndexerRPC {
        read_state,
        chain_tip_change,
        mempool_change,
    };

    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(crate::indexer::FILE_DESCRIPTOR_SET)
        .build_v1()
        .unwrap()
        .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);

    tracing::info!("Trying to open indexer RPC endpoint at {}...", listen_addr,);

    let tcp_listener = tokio::net::TcpListener::bind(listen_addr).await?;

    let listen_addr = tcp_listener.local_addr()?;
    tracing::info!("{OPENED_RPC_ENDPOINT_MSG}{}", listen_addr);

    let server_task: JoinHandle<Result<(), BoxError>> = tokio::spawn(async move {
        Server::builder()
            .max_concurrent_streams(Some(MAX_CONCURRENT_STREAMS))
            .http2_keepalive_interval(Some(HTTP2_KEEPALIVE_INTERVAL))
            .http2_keepalive_timeout(Some(HTTP2_KEEPALIVE_TIMEOUT))
            .add_service(reflection_service)
            .add_service(
                IndexerServer::new(indexer_service)
                    .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE),
            )
            .serve_with_incoming(tcp_incoming(tcp_listener))
            .await?;

        Ok(())
    });

    Ok((server_task, listen_addr))
}

/// Builds the incoming TCP stream for the indexer gRPC server with `TCP_NODELAY` enabled.
fn tcp_incoming(tcp_listener: tokio::net::TcpListener) -> TcpIncoming {
    TcpIncoming::from(tcp_listener).with_nodelay(Some(true))
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, SocketAddr},
        time::Duration,
    };

    use tokio::net::{TcpListener, TcpStream};
    use tokio_stream::StreamExt;

    use super::tcp_incoming;

    #[tokio::test]
    async fn indexer_grpc_tcp_incoming_enables_nodelay() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("ephemeral loopback port is bindable");
        let addr = listener
            .local_addr()
            .expect("bound listener always has a local address");
        let mut incoming = tcp_incoming(listener);

        let (client, accepted) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(TcpStream::connect(addr), incoming.next())
        })
        .await
        .expect("loopback connect and accept complete within timeout");

        let _client = client.expect("loopback connection to bound listener succeeds");
        let stream = accepted
            .expect("listener yields the accepted connection")
            .expect("loopback accept succeeds");

        assert!(
            stream
                .nodelay()
                .expect("accepted TCP stream supports reading TCP_NODELAY"),
            "indexer gRPC server must enable TCP_NODELAY on accepted sockets"
        );
    }
}
