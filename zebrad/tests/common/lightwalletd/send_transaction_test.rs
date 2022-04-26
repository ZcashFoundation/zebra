//! Test sending transactions using a lightwalletd instance connected to a zebrad instance.
//!
//! This test requires a cached chain state that is partially synchronized, i.e., it should be a
//! few blocks below the network chain tip height.
//!
//! The transactions to use to send are obtained from the blocks synchronized by a temporary zebrad
//! instance that are higher than the chain tip of the cached state.
//!
//! The zebrad instance connected to lightwalletd uses the cached state and does not connect to any
//! external peers, which prevents it from downloading the blocks from where the test transactions
//! were obtained. This is to ensure that zebra does not reject the transactions because they have
//! already been seen in a block.

use std::{
    collections::HashSet,
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use color_eyre::eyre::{eyre, Result};
use futures::TryFutureExt;
use tempfile::TempDir;
use tokio::fs;
use tower::{util::BoxService, Service, ServiceExt};

use zebra_chain::{
    block, chain_tip::ChainTip, parameters::Network, serialization::ZcashSerialize,
    transaction::Transaction,
};
use zebra_state::{ChainTipChange, HashOrHeight, LatestChainTip};
use zebra_test::{args, command::NO_MATCHES_REGEX_ITER, net::random_known_port, prelude::*};

use crate::{
    common::{
        config::testdir,
        launch::ZebradTestDirExt,
        lightwalletd::{self, random_known_rpc_port_config, LightWalletdTestDirExt},
        sync::{sync_until, MempoolBehavior, SYNC_FINISHED_REGEX},
    },
    LIGHTWALLETD_FAILURE_MESSAGES, LIGHTWALLETD_IGNORE_MESSAGES, PROCESS_FAILURE_MESSAGES,
    ZEBRA_FAILURE_MESSAGES,
};

/// Type alias for a boxed state service.
type BoxStateService =
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>;

/// Type alias for the RPC client to communicate with a lightwalletd instance.
type LightwalletdRpcClient = lightwalletd::rpc::compact_tx_streamer_client::CompactTxStreamerClient<
    tonic::transport::Channel,
>;

/// Optional environment variable with the cached state for lightwalletd.
///
/// Can be used to speed up the [`sending_transactions_using_lightwalletd`] test, by allowing the
/// test to reuse the cached lightwalletd synchronization data.
const LIGHTWALLETD_DATA_DIR_VAR: &str = "LIGHTWALLETD_DATA_DIR";

/// The maximum time to wait for Zebrad to synchronize up to the chain tip starting from a
/// partially synchronized state.
///
/// The partially synchronized state is expected to be close to the tip, so this timeout can be
/// lower than what's expected for a full synchronization. However, a value that's too short may
/// cause the test to fail.
const FINISH_PARTIAL_SYNC_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// The maximum time that a `lightwalletd` integration test is expected to run.
pub const LIGHTWALLETD_TEST_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// The test entry point.
pub async fn run() -> Result<()> {
    zebra_test::init();

    const CACHED_STATE_PATH_VAR: &str = "ZEBRA_CACHED_STATE_PATH";

    let cached_state_path = match env::var_os(CACHED_STATE_PATH_VAR) {
        Some(argument) => PathBuf::from(argument),
        None => {
            tracing::info!(
                "skipped send transactions using lightwalletd test, \
                 set the {CACHED_STATE_PATH_VAR:?} environment variable to run the test",
            );
            return Ok(());
        }
    };

    let network = Network::Mainnet;

    let (transactions, partial_sync_path) =
        load_transactions_from_a_future_block(network, cached_state_path).await?;

    let (_zebrad, zebra_rpc_address) = spawn_zebrad_for_rpc_without_initial_peers(
        Network::Mainnet,
        partial_sync_path,
        LIGHTWALLETD_TEST_TIMEOUT,
    )?;

    let (_lightwalletd, lightwalletd_rpc_port) =
        spawn_lightwalletd_with_rpc_server(zebra_rpc_address)?;

    let mut rpc_client = connect_to_lightwalletd(lightwalletd_rpc_port).await?;

    for transaction in transactions {
        let expected_response = lightwalletd::rpc::SendResponse {
            error_code: 0,
            error_message: format!("\"{}\"", transaction.hash()),
        };

        let request = prepare_send_transaction_request(transaction);

        let response = rpc_client.send_transaction(request).await?.into_inner();

        assert_eq!(response, expected_response);
    }

    Ok(())
}

/// Loads transactions from a block that's after the chain tip of the cached state.
///
/// This copies the cached state into a temporary directory when it is needed to avoid overwriting
/// anything. Two copies are made of the cached state.
///
/// The first copy is used by a zebrad instance connected to the network that finishes
/// synchronizing the chain. The transactions are loaded from this updated state.
///
/// The second copy of the state is returned together with the transactions. This means that the
/// returned tuple contains the temporary directory with the partially synchronized chain, and a
/// list of valid transactions that are not in any of the blocks present in that partially
/// synchronized chain.
async fn load_transactions_from_a_future_block(
    network: Network,
    cached_state_path: PathBuf,
) -> Result<(Vec<Arc<Transaction>>, TempDir)> {
    let (partial_sync_path, partial_sync_height) =
        prepare_partial_sync(network, cached_state_path).await?;

    let full_sync_path =
        perform_full_sync_starting_from(network, partial_sync_path.as_ref()).await?;

    let transactions =
        load_transactions_from_block_after(partial_sync_height, network, full_sync_path.as_ref())
            .await?;

    Ok((transactions, partial_sync_path))
}

/// Prepares the temporary directory of the partially synchronized chain.
///
/// Returns a temporary directory that can be used by a Zebra instance, as well as the chain tip
/// height of the partially synchronized chain.
async fn prepare_partial_sync(
    network: Network,
    cached_zebra_state: PathBuf,
) -> Result<(TempDir, block::Height)> {
    let partial_sync_path = copy_state_directory(cached_zebra_state).await?;
    let partial_sync_state_dir = partial_sync_path.as_ref().join("state");
    let tip_height = load_tip_height_from_state_directory(network, &partial_sync_state_dir).await?;

    Ok((partial_sync_path, tip_height))
}

/// Loads the chain tip height from the state stored in a specified directory.
async fn load_tip_height_from_state_directory(
    network: Network,
    state_path: &Path,
) -> Result<block::Height> {
    let (_state_service, _read_state_service, latest_chain_tip, _chain_tip_change) =
        start_state_service(network, state_path).await?;

    let chain_tip_height = latest_chain_tip
        .best_tip_height()
        .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))?;

    Ok(chain_tip_height)
}

/// Runs a zebrad instance to synchronize the chain to the network tip.
///
/// The zebrad instance is executed on a copy of the partially synchronized chain state. This copy
/// is returned afterwards, containing the fully synchronized chain state.
async fn perform_full_sync_starting_from(
    network: Network,
    partial_sync_path: &Path,
) -> Result<TempDir> {
    let fully_synced_path = copy_state_directory(&partial_sync_path).await?;

    sync_until(
        block::Height::MAX,
        network,
        SYNC_FINISHED_REGEX,
        FINISH_PARTIAL_SYNC_TIMEOUT,
        fully_synced_path,
        MempoolBehavior::ShouldAutomaticallyActivate,
        true,
        false,
    )
}

/// Loads transactions from a block that's after the specified `height`.
///
/// Starts at the block after the block at the specified `height`, and stops when it finds a block
/// from where it can load at least one non-coinbase transaction.
///
/// # Panics
///
/// If the specified `state_path` contains a chain state that's not synchronized to a tip that's
/// after `height`.
async fn load_transactions_from_block_after(
    height: block::Height,
    network: Network,
    state_path: &Path,
) -> Result<Vec<Arc<Transaction>>> {
    let (_read_write_state_service, mut state, latest_chain_tip, _chain_tip_change) =
        start_state_service(network, state_path.join("state")).await?;

    let tip_height = latest_chain_tip
        .best_tip_height()
        .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))?;

    assert!(
        tip_height > height,
        "Chain not synchronized to a block after the specified height"
    );

    let mut target_height = height.0;
    let mut transactions = Vec::new();

    while transactions.is_empty() {
        transactions =
            load_transactions_from_block(block::Height(target_height), &mut state).await?;

        transactions.retain(|transaction| !transaction.is_coinbase());

        target_height += 1;
    }

    Ok(transactions)
}

/// Performs a request to the provided read-only `state` service to fetch all transactions from a
/// block at the specified `height`.
async fn load_transactions_from_block<ReadStateService>(
    height: block::Height,
    state: &mut ReadStateService,
) -> Result<Vec<Arc<Transaction>>>
where
    ReadStateService: Service<
        zebra_state::ReadRequest,
        Response = zebra_state::ReadResponse,
        Error = zebra_state::BoxError,
    >,
{
    let request = zebra_state::ReadRequest::Block(HashOrHeight::Height(height));

    let response = state
        .ready()
        .and_then(|ready_service| ready_service.call(request))
        .map_err(|error| eyre!(error))
        .await?;

    let block = match response {
        zebra_state::ReadResponse::Block(Some(block)) => block,
        zebra_state::ReadResponse::Block(None) => {
            panic!("Missing block at {height:?} from state")
        }
        _ => unreachable!("Incorrect response from state service: {response:?}"),
    };

    Ok(block.transactions.to_vec())
}

/// Starts a state service using the provided `cache_dir` as the directory with the chain state.
async fn start_state_service(
    network: Network,
    cache_dir: impl Into<PathBuf>,
) -> Result<(
    BoxStateService,
    impl Service<
        zebra_state::ReadRequest,
        Response = zebra_state::ReadResponse,
        Error = zebra_state::BoxError,
    >,
    LatestChainTip,
    ChainTipChange,
)> {
    let config = zebra_state::Config {
        cache_dir: cache_dir.into(),
        ..zebra_state::Config::default()
    };

    Ok(zebra_state::init(config, network))
}

/// Spawns a zebrad instance to interact with lightwalletd, but without an internet connection.
///
/// This prevents it from downloading blocks. Instead, the `zebra_directory` parameter allows
/// providing an initial state to the zebrad instance.
pub fn spawn_zebrad_for_rpc_without_initial_peers<P: ZebradTestDirExt>(
    network: Network,
    zebra_directory: P,
    timeout: Duration,
) -> Result<(TestChild<P>, SocketAddr)> {
    let mut config = random_known_rpc_port_config()
        .expect("Failed to create a config file with a known RPC listener port");

    config.state.ephemeral = false;
    config.network.initial_mainnet_peers = HashSet::new();
    config.network.initial_testnet_peers = HashSet::new();
    config.network.network = network;
    config.mempool.debug_enable_at_height = Some(0);

    let mut zebrad = zebra_directory
        .with_config(&mut config)?
        .spawn_child(args!["start"])?
        .bypass_test_capture(true)
        .with_timeout(timeout)
        .with_failure_regex_iter(
            // TODO: replace with a function that returns the full list and correct return type
            ZEBRA_FAILURE_MESSAGES
                .iter()
                .chain(PROCESS_FAILURE_MESSAGES)
                .cloned(),
            NO_MATCHES_REGEX_ITER.iter().cloned(),
        );

    let rpc_address = config.rpc.listen_addr.unwrap();

    zebrad.expect_stdout_line_matches("activating mempool")?;
    zebrad.expect_stdout_line_matches(&format!("Opened RPC endpoint at {}", rpc_address))?;

    Ok((zebrad, rpc_address))
}

/// Start a lightwalletd instance with its RPC server functionality enabled.
///
/// Returns the lightwalletd instance and the port number that it is listening for RPC connections.
fn spawn_lightwalletd_with_rpc_server(
    zebrad_rpc_address: SocketAddr,
) -> Result<(TestChild<TempDir>, u16)> {
    let lightwalletd_dir = testdir()?.with_lightwalletd_config(zebrad_rpc_address)?;

    let lightwalletd_rpc_port = random_known_port();
    let lightwalletd_rpc_address = format!("127.0.0.1:{lightwalletd_rpc_port}");

    let mut arguments = args!["--grpc-bind-addr": lightwalletd_rpc_address];

    if let Ok(data_dir) = env::var(LIGHTWALLETD_DATA_DIR_VAR) {
        arguments.set_parameter("--data-dir", data_dir);
    }

    let mut lightwalletd = lightwalletd_dir
        .spawn_lightwalletd_child(arguments)?
        .with_timeout(LIGHTWALLETD_TEST_TIMEOUT)
        .with_failure_regex_iter(
            // TODO: replace with a function that returns the full list and correct return type
            LIGHTWALLETD_FAILURE_MESSAGES
                .iter()
                .chain(PROCESS_FAILURE_MESSAGES)
                .cloned(),
            // TODO: some exceptions do not apply to the cached state tests (#3511)
            LIGHTWALLETD_IGNORE_MESSAGES.iter().cloned(),
        );

    lightwalletd.expect_stdout_line_matches("Starting gRPC server")?;
    lightwalletd.expect_stdout_line_matches("Waiting for block")?;

    Ok((lightwalletd, lightwalletd_rpc_port))
}

/// Connect to a lightwalletd RPC instance.
async fn connect_to_lightwalletd(lightwalletd_rpc_port: u16) -> Result<LightwalletdRpcClient> {
    let lightwalletd_rpc_address = format!("http://127.0.0.1:{lightwalletd_rpc_port}");

    let rpc_client = LightwalletdRpcClient::connect(lightwalletd_rpc_address).await?;

    Ok(rpc_client)
}

/// Prepare a request to send to lightwalletd that contains a transaction to be sent.
fn prepare_send_transaction_request(
    transaction: Arc<Transaction>,
) -> lightwalletd::rpc::RawTransaction {
    let transaction_bytes = transaction.zcash_serialize_to_vec().unwrap();

    lightwalletd::rpc::RawTransaction {
        data: transaction_bytes,
        height: -1,
    }
}

/// Recursively copy a chain state directory into a new temporary directory.
async fn copy_state_directory(source: impl AsRef<Path>) -> Result<TempDir> {
    let destination = testdir()?;

    let mut remaining_directories = vec![PathBuf::from(source.as_ref())];

    while let Some(directory) = remaining_directories.pop() {
        let sub_directories =
            copy_directory(&directory, source.as_ref(), destination.as_ref()).await?;

        remaining_directories.extend(sub_directories);
    }

    Ok(destination)
}

/// Copy the contents of a directory, and return the sub-directories it contains.
///
/// Copies all files from the `directory` into the destination specified by the concatenation of
/// the `base_destination_path` and `directory` stripped of its `prefix`.
async fn copy_directory(
    directory: &Path,
    prefix: &Path,
    base_destination_path: &Path,
) -> Result<Vec<PathBuf>> {
    let mut sub_directories = Vec::new();
    let mut entries = fs::read_dir(directory).await?;

    let destination =
        base_destination_path.join(directory.strip_prefix(prefix).expect("Invalid path prefix"));

    fs::create_dir_all(&destination).await?;

    while let Some(entry) = entries.next_entry().await? {
        let entry_path = entry.path();
        let file_type = entry.file_type().await?;

        if file_type.is_file() {
            let file_name = entry_path.file_name().expect("Missing file name");
            let destination_path = destination.join(file_name);

            fs::copy(&entry_path, destination_path).await?;
        } else if file_type.is_dir() {
            sub_directories.push(entry_path);
        } else if file_type.is_symlink() {
            unimplemented!("Symbolic link support is currently not necessary");
        } else {
            panic!("Unknown file type");
        }
    }

    Ok(sub_directories)
}
