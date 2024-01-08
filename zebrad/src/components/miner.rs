//! Internal mining in Zebra.
//!
//! # TODO
//! - pause mining if we have no peers, like `zcashd` does,
//!   and add a developer config that mines regardless of how many peers we have.
//!   <https://github.com/zcash/zcash/blob/6fdd9f1b81d3b228326c9826fa10696fc516444b/src/miner.cpp#L865-L880>
//! - move common code into zebra-chain or zebra-node-services and remove the RPC dependency.

use color_eyre::Report;
use tower::Service;

use zebra_chain::{block, chain_sync_status::ChainSyncStatus, chain_tip::ChainTip};
use zebra_network::AddressBookPeers;
use zebra_node_services::mempool;
use zebra_rpc::methods::{
    get_block_template_rpcs::get_block_template::{
        self, GetBlockTemplateCapability::*, GetBlockTemplateRequestMode::*,
    },
    GetBlockTemplateRpc, GetBlockTemplateRpcImpl,
};

/// Runs a single mining thread to generate blocks, calculate equihash solutions, and submit valid
/// blocks to Zebra's block validator.
#[instrument(skip(rpc))]
pub async fn run_mining_solver<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>(
    rpc: GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>,
) -> Result<(), Report>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
{
    // Pass the correct arguments, even if Zebra currently ignores them.
    let mut long_poll_id = None;
    let mut parameters = get_block_template::JsonParameters {
        mode: Template,
        data: None,
        capabilities: vec![LongPoll, CoinbaseTxn],
        long_poll_id,
        _work_id: None,
    };

    let template = rpc.get_block_template(Some(parameters)).await;

    Ok(())
}
