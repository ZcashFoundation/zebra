//! A tonic gRPC server for the lightwalletd `CompactTxStreamer` client API.

use std::{net::SocketAddr, time::Duration};

use tokio::task::JoinHandle;
use tonic::transport::{server::TcpIncoming, Server};
use tower::BoxError;
use zebra_chain::{chain_tip::ChainTip, parameters::Network};
use zebra_node_services::mempool::{MempoolService, MempoolTxSubscriber};
use zebra_state::ReadState;

use crate::{
    lightwalletd::{
        compact_tx_streamer_server::CompactTxStreamerServer, tracing::GrpcTracingLayer,
    },
    methods::RpcServer as RpcMethods,
    server::OPENED_LIGHTWALLETD_ENDPOINT_MSG,
};

/// Maximum concurrent HTTP/2 streams per connection.
///
/// Must be set explicitly: tonic defaults it to `None`, which removes hyper's own
/// 200-stream default rather than applying it. Matches the indexer server.
const MAX_CONCURRENT_STREAMS: u32 = 20;

/// Interval between HTTP/2 keepalive pings sent to detect dead connections.
const HTTP2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Timeout for HTTP/2 keepalive pings before the connection is closed.
const HTTP2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

type ServerTask = JoinHandle<Result<(), BoxError>>;

/// Lightwalletd gRPC service.
///
/// Most methods are implemented by calling Zebra's JSON-RPC methods in-process, in the
/// same way lightwalletd calls them on zcashd. Compact blocks and mempool streams are
/// built directly from the state and mempool services.
pub struct LightwalletdRPC<Rpc, ReadStateService, Mempool, Tip>
where
    Rpc: RpcMethods + Clone,
    ReadStateService: ReadState,
    Mempool: MempoolService,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    pub(super) rpc: Rpc,
    pub(super) read_state: ReadStateService,
    pub(super) mempool: Mempool,
    pub(super) latest_chain_tip: Tip,
    pub(super) mempool_change: MempoolTxSubscriber,
    pub(super) network: Network,
}

/// Initializes the lightwalletd gRPC server.
#[tracing::instrument(skip_all)]
pub async fn init<Rpc, ReadStateService, Mempool, Tip>(
    listen_addr: SocketAddr,
    rpc: Rpc,
    read_state: ReadStateService,
    mempool: Mempool,
    latest_chain_tip: Tip,
    mempool_change: MempoolTxSubscriber,
    network: Network,
) -> Result<(ServerTask, SocketAddr), BoxError>
where
    Rpc: RpcMethods + Clone,
    ReadStateService: ReadState,
    Mempool: MempoolService,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    let lightwalletd_service = LightwalletdRPC {
        rpc,
        read_state,
        mempool,
        latest_chain_tip,
        mempool_change,
        network,
    };

    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(crate::lightwalletd::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    tracing::info!(
        "Trying to open lightwalletd gRPC endpoint at {}...",
        listen_addr,
    );

    let tcp_listener = tokio::net::TcpListener::bind(listen_addr).await?;

    let listen_addr = tcp_listener.local_addr()?;
    tracing::info!("{OPENED_LIGHTWALLETD_ENDPOINT_MSG}{}", listen_addr);

    let server_task: JoinHandle<Result<(), BoxError>> = tokio::spawn(async move {
        Server::builder()
            .max_concurrent_streams(Some(MAX_CONCURRENT_STREAMS))
            .http2_keepalive_interval(Some(HTTP2_KEEPALIVE_INTERVAL))
            .http2_keepalive_timeout(Some(HTTP2_KEEPALIVE_TIMEOUT))
            .layer(GrpcTracingLayer)
            .add_service(reflection_service)
            .add_service(CompactTxStreamerServer::new(lightwalletd_service))
            .serve_with_incoming(tcp_incoming(tcp_listener))
            .await?;

        Ok(())
    });

    Ok((server_task, listen_addr))
}

/// Builds the incoming TCP stream for the lightwalletd gRPC server with `TCP_NODELAY` enabled.
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
    async fn lightwalletd_grpc_tcp_incoming_enables_nodelay() {
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
            "lightwalletd gRPC server must enable TCP_NODELAY on accepted sockets"
        );
    }
}
