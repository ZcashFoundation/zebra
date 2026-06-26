//! Bootstrapping a read-state follower from a co-located node's indexer gRPC.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use tokio::task::JoinHandle;
use tower::BoxError;
use zebra_chain::parameters::Network;
use zebra_state::{
    spawn_init_read_only, state_database_format_version_in_code, ChainTipChange, Config,
    LatestChainTip, ReadStateService,
};

use crate::indexer::{indexer_client::IndexerClient, Empty};

use super::syncer::TrustedChainSync;

/// How long to wait to connect to the indexer gRPC and fetch state info before giving up.
const STATE_INFO_TIMEOUT: Duration = Duration::from_secs(30);

/// Decoded `GetStateInfo` response: everything the follower needs to open the primary's db.
struct StateInfoDecoded {
    network: Network,
    db_path: PathBuf,
}

/// Errors bootstrapping the follower from the primary's `GetStateInfo`.
#[derive(Debug, thiserror::Error)]
enum InitError {
    #[error("failed to connect to indexer gRPC at {addr}: {source}")]
    Connect {
        addr: SocketAddr,
        source: tonic::transport::Error,
    },
    #[error("connecting to indexer gRPC and fetching state info timed out")]
    Timeout,
    #[error("GetStateInfo request failed: {0}")]
    Request(#[from] tonic::Status),
    #[error("failed to parse primary state format version {version:?}: {source}")]
    Version {
        version: String,
        source: semver::Error,
    },
    // `InvalidNetworkError` is not a public type (the `network` module is private), so the
    // parse error is carried as a message string rather than as the error type.
    #[error("failed to parse primary network {network:?}: {message}")]
    Network { network: String, message: String },
    #[error(
        "incompatible state database major version: follower runs {ours}, primary reports {theirs}"
    )]
    IncompatibleVersion { ours: u64, theirs: u64 },
}

/// Connects to the indexer gRPC, calls `GetStateInfo`, and decodes a [`StateInfoDecoded`],
/// validating that the follower's db format major version matches the primary's.
async fn fetch_state_info(addr: SocketAddr) -> Result<StateInfoDecoded, InitError> {
    let response = tokio::time::timeout(STATE_INFO_TIMEOUT, async {
        let mut client = IndexerClient::connect(format!("http://{addr}"))
            .await
            .map_err(|source| InitError::Connect { addr, source })?;
        Ok::<_, InitError>(client.get_state_info(Empty {}).await?.into_inner())
    })
    .await
    .map_err(|_| InitError::Timeout)??;

    // Bounded, safe decode: tonic enforces the max message size on the wire; each parse below is
    // over an already-bounded `String`. None of this trusts unbounded attacker input.
    let db_format_version: semver::Version =
        response
            .db_format_version
            .parse()
            .map_err(|source| InitError::Version {
                version: response.db_format_version.clone(),
                source,
            })?;
    let network = response
        .network
        .parse::<Network>()
        .map_err(|err| InitError::Network {
            network: response.network.clone(),
            message: err.to_string(),
        })?;

    let ours = state_database_format_version_in_code().major;
    if ours != db_format_version.major {
        return Err(InitError::IncompatibleVersion {
            ours,
            theirs: db_format_version.major,
        });
    }

    Ok(StateInfoDecoded {
        network,
        db_path: PathBuf::from(response.db_path),
    })
}

/// Connects to a co-located Zebra node's indexer gRPC at `indexer_rpc_address`, fetches its
/// state info, opens that node's finalized database **read-only at its live path** (supporting an
/// ephemeral node's temp dir), and spawns a [`TrustedChainSync`] to follow its best chain.
///
/// Returns a [`ReadStateService`], [`LatestChainTip`], [`ChainTipChange`], and the sync task handle.
pub fn init_read_state_with_syncer(
    indexer_rpc_address: SocketAddr,
) -> JoinHandle<
    Result<
        (
            ReadStateService,
            LatestChainTip,
            ChainTipChange,
            JoinHandle<()>,
        ),
        BoxError,
    >,
> {
    tokio::spawn(async move {
        let StateInfoDecoded { network, db_path } = fetch_state_info(indexer_rpc_address).await?;

        // The follower opens the primary's db read-only at its exact runtime path. It does NOT
        // reuse the primary's config: `with_read_only_db_path` forces `ephemeral = false` and
        // disables old-db cleanup, so this is always a read-only secondary that never deletes the
        // primary's files, regardless of whether the primary is ephemeral.
        let config = Config::default().with_read_only_db_path(db_path);

        // Outer `?`: JoinError if the blocking open panicked; inner `?`: StateInitError.
        let (read_state, db, non_finalized_state_sender) =
            spawn_init_read_only(config, &network).await??;
        let (latest_chain_tip, chain_tip_change, sync_task) =
            TrustedChainSync::spawn(indexer_rpc_address, db, non_finalized_state_sender).await?;

        Ok((read_state, latest_chain_tip, chain_tip_change, sync_task))
    })
}
