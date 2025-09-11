//! Client for Zebra's RPC server.
//! Types, constants, and functions needed by clients of Zebra's RPC server
//
// Re-exports types to build the API. We want to flatten the module substructure
// which is split mostly to keep file sizes small. Additionally, we re-export
// types from other crates which are exposed in the API.
//
// TODO: Move `hex_data` and `trees` modules to/under the `types` module?

pub use zebra_chain;

#[allow(deprecated)]
pub use crate::methods::{
    hex_data::HexData,
    trees::{
        Commitments, GetSubtreesByIndexResponse, GetTreestateResponse, SubtreeRpcData, Treestate,
    },
    types::{
        default_roots::DefaultRoots,
        get_block_template::{
            BlockProposalResponse, BlockTemplateResponse, BlockTemplateTimeSource,
            GetBlockTemplateCapability, GetBlockTemplateParameters, GetBlockTemplateRequestMode,
            GetBlockTemplateResponse,
        },
        get_blockchain_info::GetBlockchainInfoBalance,
        get_mining_info::GetMiningInfoResponse,
        get_raw_mempool::{GetRawMempoolResponse, MempoolObject},
        network_info::NetworkInfo,
        peer_info::{GetPeerInfoResponse, PeerInfo},
        submit_block::{SubmitBlockErrorResponse, SubmitBlockResponse},
        subsidy::{BlockSubsidy, FundingStream, GetBlockSubsidyResponse},
        transaction::{
            Input, JoinSplit, Orchard, OrchardAction, OrchardFlags, Output, ScriptPubKey,
            ScriptSig, ShieldedOutput, ShieldedSpend, TransactionObject, TransactionTemplate,
        },
        unified_address::ZListUnifiedReceiversResponse,
        validate_address::ValidateAddressResponse,
        z_validate_address::{ZValidateAddressResponse, ZValidateAddressType},
    },
    AddressStrings, BlockHeaderObject, BlockObject, GetAddressBalanceRequest,
    GetAddressBalanceResponse, GetAddressTxIdsRequest, GetAddressUtxosResponse,
    GetAddressUtxosResponseObject, GetBlockHashResponse, GetBlockHeaderResponse,
    GetBlockHeightAndHashResponse, GetBlockResponse, GetBlockTransaction, GetBlockTrees,
    GetBlockchainInfoResponse, GetInfoResponse, GetRawTransactionResponse, Hash,
    SendRawTransactionResponse, Utxo,
};

/// Constants needed by clients of Zebra's RPC server
// TODO: Export all other constants?
pub use crate::methods::types::long_poll::LONG_POLL_ID_LENGTH;
