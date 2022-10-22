//! RPC methods related to mining only available with `getblocktemplate-rpcs` rust feature.
use zebra_chain::{block::Height, chain_tip::ChainTip};

use futures::{FutureExt, TryFutureExt};
use jsonrpc_core::{self, BoxFuture, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tower::{Service, ServiceExt};

use crate::methods::{GetBlockHash, MISSING_BLOCK_ERROR_CODE};

/// getblocktemplate RPC method signatures.
#[rpc(server)]
pub trait GetBlockTemplateRpc {
    /// Returns the height of the most recent block in the best valid block chain (equivalently,
    /// the number of blocks in this chain excluding the genesis block).
    ///
    /// zcashd reference: [`getblockcount`](https://zcash.github.io/rpc/getblockcount.html)
    ///
    /// # Notes
    ///
    /// This rpc method is available only if zebra is built with `--features getblocktemplate-rpcs`.
    #[rpc(name = "getblockcount")]
    fn get_block_count(&self) -> Result<u32>;

    /// Returns the hash of the block of a given height iff the index argument correspond
    /// to a block in the best chain.
    ///
    /// zcashd reference: [`getblockhash`](https://zcash-rpc.github.io/getblockhash.html)
    ///
    /// # Parameters
    ///
    /// - `index`: (numeric, required) The block index.
    ///
    /// # Notes
    ///
    /// - If `index` is positive then index = block height.
    /// - If `index` is negative then -1 is the last known valid block.
    /// - This rpc method is available only if zebra is built with `--features getblocktemplate-rpcs`.
    #[rpc(name = "getblockhash")]
    fn get_block_hash(&self, index: i32) -> BoxFuture<Result<GetBlockHash>>;

    /// Documentation to be filled as we go.
    ///
    /// zcashd reference: [`getblocktemplate`](https://zcash-rpc.github.io/getblocktemplate.html)
    ///
    /// # Notes
    ///
    /// - This rpc method is available only if zebra is built with `--features getblocktemplate-rpcs`.
    #[rpc(name = "getblocktemplate")]
    fn get_block_template(&self) -> BoxFuture<Result<GetBlockTemplate>>;
}

/// RPC method implementations.
pub struct GetBlockTemplateRpcImpl<Tip, State>
where
    Tip: ChainTip,
    State: Service<
        zebra_state::ReadRequest,
        Response = zebra_state::ReadResponse,
        Error = zebra_state::BoxError,
    >,
{
    // TODO: Add the other fields from the [`Rpc`] struct as-needed
    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: Tip,

    /// A handle to the state service.
    state: State,
}

impl<Tip, State> GetBlockTemplateRpcImpl<Tip, State>
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    /// Create a new instance of the RPC handler.
    pub fn new(latest_chain_tip: Tip, state: State) -> Self {
        Self {
            latest_chain_tip,
            state,
        }
    }
}

impl<Tip, State> GetBlockTemplateRpc for GetBlockTemplateRpcImpl<Tip, State>
where
    Tip: ChainTip + Send + Sync + 'static,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
{
    fn get_block_count(&self) -> Result<u32> {
        self.latest_chain_tip
            .best_tip_height()
            .map(|height| height.0)
            .ok_or(Error {
                code: ErrorCode::ServerError(0),
                message: "No blocks in state".to_string(),
                data: None,
            })
    }

    fn get_block_hash(&self, index: i32) -> BoxFuture<Result<GetBlockHash>> {
        let mut state = self.state.clone();

        let maybe_tip_height = self.latest_chain_tip.best_tip_height();

        async move {
            let tip_height = maybe_tip_height.ok_or(Error {
                code: ErrorCode::ServerError(0),
                message: "No blocks in state".to_string(),
                data: None,
            })?;

            let height = get_height_from_int(index, tip_height)?;

            let request = zebra_state::ReadRequest::BestChainBlockHash(height);
            let response = state
                .ready()
                .and_then(|service| service.call(request))
                .await
                .map_err(|error| Error {
                    code: ErrorCode::ServerError(0),
                    message: error.to_string(),
                    data: None,
                })?;

            match response {
                zebra_state::ReadResponse::BlockHash(Some(hash)) => Ok(GetBlockHash(hash)),
                zebra_state::ReadResponse::BlockHash(None) => Err(Error {
                    code: MISSING_BLOCK_ERROR_CODE,
                    message: "Block not found".to_string(),
                    data: None,
                }),
                _ => unreachable!("unmatched response to a block request"),
            }
        }
        .boxed()
    }

    fn get_block_template(&self) -> BoxFuture<Result<GetBlockTemplate>> {
        async move {
            let empty_string = String::from("");

            // Returns empty `GetBlockTemplate`
            Ok(GetBlockTemplate {
                capabilities: vec![],
                version: 0,
                previousblockhash: empty_string.clone(),
                blockcommitmentshash: empty_string.clone(),
                lightclientroothash: empty_string.clone(),
                finalsaplingroothash: empty_string.clone(),
                defaultroots: DefaultRoots {
                    merkleroot: empty_string.clone(),
                    chainhistoryroot: empty_string.clone(),
                    authdataroot: empty_string.clone(),
                    blockcommitmentshash: empty_string.clone(),
                },
                transactions: vec![],
                coinbasetxn: Coinbase {},
                longpollid: empty_string.clone(),
                target: empty_string.clone(),
                mintime: 0,
                mutable: vec![],
                noncerange: empty_string.clone(),
                sigoplimit: 0,
                sizelimit: 0,
                curtime: 0,
                bits: empty_string,
                height: 0,
            })
        }
        .boxed()
    }
}
/// Documentation to be added after we document all the individual fields.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetBlockTemplate {
    /// Add documentation.
    pub capabilities: Vec<String>,
    /// Add documentation.
    pub version: usize,
    /// Add documentation.
    pub previousblockhash: String,
    /// Add documentation.
    pub blockcommitmentshash: String,
    /// Add documentation.
    pub lightclientroothash: String,
    /// Add documentation.
    pub finalsaplingroothash: String,
    /// Add documentation.
    pub defaultroots: DefaultRoots,
    /// Add documentation.
    pub transactions: Vec<Transaction>,
    /// Add documentation.
    pub coinbasetxn: Coinbase,
    /// Add documentation.
    pub longpollid: String,
    /// Add documentation.
    pub target: String,
    /// Add documentation.
    pub mintime: u32,
    /// Add documentation.
    pub mutable: Vec<String>,
    /// Add documentation.
    pub noncerange: String,
    /// Add documentation.
    pub sigoplimit: u32,
    /// Add documentation.
    pub sizelimit: u32,
    /// Add documentation.
    pub curtime: u32,
    /// Add documentation.
    pub bits: String,
    /// Add documentation.
    pub height: u32,
}

/// Documentation to be added in #5452 or #5455.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DefaultRoots {
    /// Add documentation.
    pub merkleroot: String,
    /// Add documentation.
    pub chainhistoryroot: String,
    /// Add documentation.
    pub authdataroot: String,
    /// Add documentation.
    pub blockcommitmentshash: String,
}

/// Documentation and fields to be added in #5454.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transaction {}

/// documentation and fields to be added in #5453.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Coinbase {}

/// Given a potentially negative index, find the corresponding `Height`.
///
/// This function is used to parse the integer index argument of `get_block_hash`.
fn get_height_from_int(index: i32, tip_height: Height) -> Result<Height> {
    if index >= 0 {
        let height = index.try_into().expect("Positive i32 always fits in u32");
        if height > tip_height.0 {
            return Err(Error::invalid_params(
                "Provided index is greater than the current tip",
            ));
        }
        Ok(Height(height))
    } else {
        // `index + 1` can't overflow, because `index` is always negative here.
        let height = i32::try_from(tip_height.0)
            .expect("tip height fits in i32, because Height::MAX fits in i32")
            .checked_add(index + 1);

        let sanitized_height = match height {
            None => return Err(Error::invalid_params("Provided index is not valid")),
            Some(h) => {
                if h < 0 {
                    return Err(Error::invalid_params(
                        "Provided negative index ends up with a negative height",
                    ));
                }
                let h: u32 = h.try_into().expect("Positive i32 always fits in u32");
                if h > tip_height.0 {
                    return Err(Error::invalid_params(
                        "Provided index is greater than the current tip",
                    ));
                }

                h
            }
        };

        Ok(Height(sanitized_height))
    }
}
