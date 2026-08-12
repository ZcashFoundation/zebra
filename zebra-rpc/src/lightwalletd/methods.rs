//! Implements `CompactTxStreamer` methods on the `LightwalletdRPC` type.

use std::{collections::HashSet, pin::Pin};

use futures::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tower::util::ServiceExt;

use zebra_chain::{
    block,
    chain_tip::ChainTip,
    parameters::Network,
    serialization::{BytesInDisplayOrder, ZcashSerialize},
    subtree::NoteCommitmentSubtreeIndex,
    transaction, transparent,
};
use zebra_node_services::mempool::{self, MempoolService};
use zebra_state::{HashOrHeight, ReadRequest, ReadResponse, ReadState};

use crate::methods::{
    trees::GetTreestateResponse, GetAddressBalanceRequest, GetAddressTxIdsRequest,
    GetAddressUtxosRequest, GetAddressUtxosResponse, RpcServer as RpcMethods,
};

use super::{
    compact_tx_streamer_server::CompactTxStreamer, server::LightwalletdRPC, Address, AddressList,
    Balance, BlockId, BlockRange, ChainSpec, CompactBlock, CompactTx, Duration, Empty, Exclude,
    GetAddressUtxosArg, GetAddressUtxosReply, GetAddressUtxosReplyList, GetSubtreeRootsArg,
    LightdInfo, PingResponse, RawTransaction, SendResponse, ShieldedProtocol, SubtreeRoot,
    TransparentAddressBlockFilter, TreeState, TxFilter,
};

/// The maximum number of messages that can be queued to be streamed to a client.
const RESPONSE_BUFFER_SIZE: usize = 64;

/// The maximum number of addresses a client can send in one request.
///
/// Bounds the addresses buffered from a client stream, and the work a single unary
/// request can start.
const MAX_REQUEST_ADDRESSES: usize = 10_000;

/// The maximum number of `exclude` entries a `GetMempoolTx` client can send.
///
/// Every entry is matched against every mempool transaction, so an unbounded list
/// lets one request occupy a runtime thread for an arbitrary time.
const MAX_EXCLUDE_ENTRIES: usize = 1_000;

/// The maximum size of a raw transaction submitted by a client.
///
/// A transaction larger than a block can never be mined.
const MAX_RAW_TRANSACTION_BYTES: usize = block::MAX_BLOCK_BYTES as usize;

/// How long to wait for a backpressured send to a response stream before treating
/// the consumer as hung and dropping the stream.
///
/// Without a bound, a consumer whose connection is half-open (dead TCP not yet
/// detected) would block the streaming task indefinitely.
const STREAM_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// A type alias for the boxed response streams returned by server-streaming methods.
type ResponseStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl<Rpc, ReadStateService, Mempool, Tip> CompactTxStreamer
    for LightwalletdRPC<Rpc, ReadStateService, Mempool, Tip>
where
    Rpc: RpcMethods + Clone,
    ReadStateService: ReadState,
    Mempool: MempoolService,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type GetBlockRangeStream = ResponseStream<CompactBlock>;
    type GetBlockRangeNullifiersStream = ResponseStream<CompactBlock>;
    type GetTaddressTxidsStream = ResponseStream<RawTransaction>;
    type GetTaddressTransactionsStream = ResponseStream<RawTransaction>;
    type GetMempoolTxStream = ResponseStream<CompactTx>;
    type GetMempoolStreamStream = ResponseStream<RawTransaction>;
    type GetSubtreeRootsStream = ResponseStream<SubtreeRoot>;
    type GetAddressUtxosStreamStream = ResponseStream<GetAddressUtxosReply>;

    async fn get_latest_block(
        &self,
        _request: Request<ChainSpec>,
    ) -> Result<Response<BlockId>, Status> {
        let (height, hash) = self
            .latest_chain_tip
            .best_tip_height_and_hash()
            .ok_or_else(|| Status::unavailable("no chain tip available yet"))?;

        Ok(Response::new(BlockId {
            height: height.0.into(),
            hash: hash.0.to_vec(),
        }))
    }

    async fn get_block(&self, request: Request<BlockId>) -> Result<Response<CompactBlock>, Status> {
        let hash_or_height = block_id_to_hash_or_height(request.into_inner())?;
        let block = compact_block(self.read_state.clone(), hash_or_height, false).await?;

        Ok(Response::new(block))
    }

    async fn get_block_nullifiers(
        &self,
        request: Request<BlockId>,
    ) -> Result<Response<CompactBlock>, Status> {
        let hash_or_height = block_id_to_hash_or_height(request.into_inner())?;
        let block = compact_block(self.read_state.clone(), hash_or_height, true).await?;

        Ok(Response::new(block))
    }

    async fn get_block_range(
        &self,
        request: Request<BlockRange>,
    ) -> Result<Response<Self::GetBlockRangeStream>, Status> {
        let stream = block_range_stream(self.read_state.clone(), request.into_inner(), false)?;

        Ok(Response::new(stream))
    }

    async fn get_block_range_nullifiers(
        &self,
        request: Request<BlockRange>,
    ) -> Result<Response<Self::GetBlockRangeNullifiersStream>, Status> {
        let stream = block_range_stream(self.read_state.clone(), request.into_inner(), true)?;

        Ok(Response::new(stream))
    }

    async fn get_transaction(
        &self,
        request: Request<TxFilter>,
    ) -> Result<Response<RawTransaction>, Status> {
        let tx_filter = request.into_inner();

        // Only specification by transaction hash is supported, matching lightwalletd.
        let hash: [u8; 32] = tx_filter.hash.try_into().map_err(|hash: Vec<u8>| {
            Status::invalid_argument(format!(
                "transaction hash must be 32 bytes, got {}",
                hash.len()
            ))
        })?;
        let hash = transaction::Hash::from(hash);

        // Look for the transaction in any chain first, then in the mempool.
        let side_chain_tx = match self
            .read_state
            .clone()
            .oneshot(ReadRequest::AnyChainTransaction(hash))
            .await
        {
            Ok(ReadResponse::AnyChainTransaction(Some(zebra_state::AnyTx::Mined(mined_tx)))) => {
                return Ok(Response::new(RawTransaction {
                    data: serialize_transaction(&mined_tx.tx)?,
                    height: mined_tx.height.0.into(),
                }));
            }
            Ok(ReadResponse::AnyChainTransaction(Some(zebra_state::AnyTx::Side((tx, _))))) => {
                Some(tx)
            }
            Ok(ReadResponse::AnyChainTransaction(None)) => None,
            Ok(_) => unreachable!("unexpected response type from ReadStateService"),
            Err(error) => {
                return Err(Status::internal(format!(
                    "failed to read transaction: {error}"
                )));
            }
        };

        let mempool_response = self
            .mempool
            .clone()
            .oneshot(mempool::Request::TransactionsByMinedId(
                [hash].into_iter().collect(),
            ))
            .await
            .map_err(|error| Status::internal(format!("failed to query mempool: {error}")))?;

        let mempool_tx = match mempool_response {
            mempool::Response::Transactions(transactions) => transactions.into_iter().next(),
            _ => unreachable!("unexpected response type from mempool service"),
        };

        if let Some(tx) = mempool_tx {
            Ok(Response::new(RawTransaction {
                data: serialize_transaction(&tx.transaction)?,
                // A height of 0 means the transaction is in the mempool.
                height: 0,
            }))
        } else if let Some(tx) = side_chain_tx {
            Ok(Response::new(RawTransaction {
                data: serialize_transaction(&tx)?,
                // The protocol's sentinel height for a transaction that has been
                // mined on a fork that is not currently the main chain.
                height: u64::MAX,
            }))
        } else {
            Err(Status::not_found("transaction not found"))
        }
    }

    async fn send_transaction(
        &self,
        request: Request<RawTransaction>,
    ) -> Result<Response<SendResponse>, Status> {
        let data = request.into_inner().data;

        // Reject before hex-encoding doubles it: a transaction larger than a block
        // can never be mined.
        if data.len() > MAX_RAW_TRANSACTION_BYTES {
            return Err(Status::invalid_argument(format!(
                "transaction too large: the limit is {MAX_RAW_TRANSACTION_BYTES} bytes"
            )));
        }

        let transaction_hex = hex::encode(data);

        // Like lightwalletd, submission errors are returned in the `SendResponse`
        // rather than as a gRPC error status.
        let response = match self.rpc.send_raw_transaction(transaction_hex, None).await {
            Ok(sent_tx) => SendResponse {
                error_code: 0,
                error_message: sent_tx.hash().to_string(),
            },
            Err(error) => SendResponse {
                error_code: error.code(),
                error_message: error.message().to_string(),
            },
        };

        Ok(Response::new(response))
    }

    async fn get_taddress_txids(
        &self,
        request: Request<TransparentAddressBlockFilter>,
    ) -> Result<Response<Self::GetTaddressTxidsStream>, Status> {
        let stream =
            taddress_transactions_stream(&self.rpc, self.read_state.clone(), request.into_inner())
                .await?;

        Ok(Response::new(stream))
    }

    async fn get_taddress_transactions(
        &self,
        request: Request<TransparentAddressBlockFilter>,
    ) -> Result<Response<Self::GetTaddressTransactionsStream>, Status> {
        let stream =
            taddress_transactions_stream(&self.rpc, self.read_state.clone(), request.into_inner())
                .await?;

        Ok(Response::new(stream))
    }

    async fn get_taddress_balance(
        &self,
        request: Request<AddressList>,
    ) -> Result<Response<Balance>, Status> {
        let addresses = request.into_inner().addresses;

        address_balance(&self.rpc, addresses)
            .await
            .map(Response::new)
    }

    async fn get_taddress_balance_stream(
        &self,
        request: Request<Streaming<Address>>,
    ) -> Result<Response<Balance>, Status> {
        let addresses = collect_streamed_addresses(request.into_inner()).await?;

        address_balance(&self.rpc, addresses)
            .await
            .map(Response::new)
    }

    async fn get_mempool_tx(
        &self,
        request: Request<Exclude>,
    ) -> Result<Response<Self::GetMempoolTxStream>, Status> {
        let exclude = request.into_inner().txid;

        if exclude.len() > MAX_EXCLUDE_ENTRIES {
            return Err(Status::invalid_argument(format!(
                "too many exclude entries: the limit is {MAX_EXCLUDE_ENTRIES}"
            )));
        }

        let transactions = mempool_transactions(self.mempool.clone()).await?;

        let txids: Vec<transaction::Hash> =
            transactions.iter().map(|tx| tx.id.mined_id()).collect();
        let excluded = excluded_txids(&txids, &exclude);

        let compact_txs: Vec<Result<CompactTx, Status>> = transactions
            .iter()
            .filter(|tx| !excluded.contains(&tx.id.mined_id()))
            .filter(|tx| super::has_compact_data(&tx.transaction))
            .map(|tx| {
                Ok(CompactTx::from_transaction(
                    0,
                    tx.id.mined_id(),
                    &tx.transaction,
                    false,
                ))
            })
            .collect();

        Ok(Response::new(Box::pin(futures::stream::iter(compact_txs))))
    }

    async fn get_mempool_stream(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::GetMempoolStreamStream>, Status> {
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);

        // Subscribe to mempool changes before taking the initial snapshot, so no
        // transactions are missed between the snapshot and the change stream.
        let mut mempool_change = self.mempool_change.subscribe();
        let mempool = self.mempool.clone();
        let mut latest_chain_tip = self.latest_chain_tip.clone();

        // The stream must only close when a block is added to the best chain after the
        // stream starts. Mark the current tip here rather than in the spawned task: a
        // block arriving before that task is first polled would otherwise be marked as
        // seen, and the stream would stay open through a block change.
        latest_chain_tip.mark_best_tip_seen();

        tokio::spawn(async move {
            let mut sent_txids: HashSet<transaction::Hash> = HashSet::new();

            // Send the current mempool contents.
            if !send_unseen_mempool_txs(&mempool, &response_sender, &mut sent_txids).await {
                return;
            }

            // Stream newly added transactions, and close the stream when a new block
            // is added to the best chain, matching lightwalletd's behaviour.
            loop {
                tokio::select! {
                    _ = latest_chain_tip.best_tip_changed() => return,
                    change = mempool_change.recv() => {
                        let change = match change {
                            Ok(change) => change,
                            // Lagging means changes were dropped, so this stream would
                            // silently omit the transactions they carried. Re-snapshot
                            // the mempool and send whatever the client hasn't seen.
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                if !send_unseen_mempool_txs(
                                    &mempool,
                                    &response_sender,
                                    &mut sent_txids,
                                )
                                .await
                                {
                                    return;
                                }

                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        };
                        if !change.is_added() {
                            continue;
                        }

                        let request = mempool::Request::TransactionsById(change.into_tx_ids());
                        let Ok(mempool::Response::Transactions(transactions)) =
                            mempool.clone().oneshot(request).await
                        else {
                            return;
                        };

                        for tx in transactions {
                            if sent_txids.contains(&tx.id.mined_id()) {
                                continue;
                            }
                            if !send_raw_mempool_tx(&response_sender, &tx).await {
                                return;
                            }
                            sent_txids.insert(tx.id.mined_id());
                        }
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn get_tree_state(
        &self,
        request: Request<BlockId>,
    ) -> Result<Response<TreeState>, Status> {
        let hash_or_height = block_id_to_hash_or_height(request.into_inner())?;

        let treestate = self
            .rpc
            .z_get_treestate(hash_or_height.to_string())
            .await
            .map_err(rpc_error_to_status)?;

        Ok(Response::new(treestate_to_proto(&self.network, treestate)))
    }

    async fn get_latest_tree_state(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<TreeState>, Status> {
        // Read the treestate by the tip's hash rather than its height, so a reorg
        // between the two reads can't return the treestate of a different block.
        let (_height, hash) = self
            .latest_chain_tip
            .best_tip_height_and_hash()
            .ok_or_else(|| Status::unavailable("no chain tip available yet"))?;

        let treestate = self
            .rpc
            .z_get_treestate(hash.to_string())
            .await
            .map_err(rpc_error_to_status)?;

        Ok(Response::new(treestate_to_proto(&self.network, treestate)))
    }

    async fn get_subtree_roots(
        &self,
        request: Request<GetSubtreeRootsArg>,
    ) -> Result<Response<Self::GetSubtreeRootsStream>, Status> {
        let args = request.into_inner();

        let pool = match ShieldedProtocol::try_from(args.shielded_protocol) {
            Ok(ShieldedProtocol::Sapling) => "sapling",
            Ok(ShieldedProtocol::Orchard) => "orchard",
            Err(_) => return Err(Status::invalid_argument("unknown shielded protocol")),
        };

        let start_index = u16::try_from(args.start_index)
            .map(NoteCommitmentSubtreeIndex)
            .map_err(|_| Status::invalid_argument("start index must fit in 16 bits"))?;

        let limit = if args.max_entries == 0 {
            None
        } else {
            // Cast is safe due to the `min` with a 16-bit value.
            Some(NoteCommitmentSubtreeIndex(
                args.max_entries.min(u16::MAX.into()) as u16,
            ))
        };

        let subtrees = self
            .rpc
            .z_get_subtrees_by_index(pool.to_string(), start_index, limit)
            .await
            .map_err(rpc_error_to_status)?;

        let read_state = self.read_state.clone();
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);

        tokio::spawn(async move {
            for subtree in subtrees.subtrees() {
                let root_hash = match hex::decode(&subtree.root) {
                    Ok(root_hash) => root_hash,
                    Err(_) => {
                        let _ = send_bounded(
                            &response_sender,
                            Err(Status::internal("invalid subtree root hex")),
                        )
                        .await;
                        return;
                    }
                };

                let completing_block_hash = match read_state
                    .clone()
                    .oneshot(ReadRequest::BestChainBlockHash(subtree.end_height))
                    .await
                {
                    Ok(ReadResponse::BlockHash(Some(hash))) => hash,
                    Ok(ReadResponse::BlockHash(None)) => {
                        let _ = send_bounded(
                            &response_sender,
                            Err(Status::not_found("completing block not found")),
                        )
                        .await;
                        return;
                    }
                    Ok(_) => unreachable!("unexpected response type from ReadStateService"),
                    Err(error) => {
                        let _ = send_bounded(
                            &response_sender,
                            Err(Status::internal(format!(
                                "failed to read completing block hash: {error}"
                            ))),
                        )
                        .await;
                        return;
                    }
                };

                let subtree_root = SubtreeRoot {
                    root_hash,
                    // Display order, unlike the other hashes here: lightwalletd reverses
                    // this one in `GetSubtreeRoots`.
                    completing_block_hash: completing_block_hash.bytes_in_display_order().to_vec(),
                    completing_block_height: subtree.end_height.0.into(),
                };

                if !send_bounded(&response_sender, Ok(subtree_root)).await {
                    return;
                }
            }
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn get_address_utxos(
        &self,
        request: Request<GetAddressUtxosArg>,
    ) -> Result<Response<GetAddressUtxosReplyList>, Status> {
        let address_utxos = address_utxos(&self.rpc, request.into_inner()).await?;

        Ok(Response::new(GetAddressUtxosReplyList { address_utxos }))
    }

    async fn get_address_utxos_stream(
        &self,
        request: Request<GetAddressUtxosArg>,
    ) -> Result<Response<Self::GetAddressUtxosStreamStream>, Status> {
        let address_utxos = address_utxos(&self.rpc, request.into_inner()).await?;

        Ok(Response::new(Box::pin(futures::stream::iter(
            address_utxos.into_iter().map(Ok),
        ))))
    }

    async fn get_lightd_info(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<LightdInfo>, Status> {
        let blockchain_info = self
            .rpc
            .get_blockchain_info()
            .await
            .map_err(rpc_error_to_status)?;
        let info = self.rpc.get_info().await.map_err(rpc_error_to_status)?;

        let (tip_branch_id, _next_branch_id) = blockchain_info.consensus().into_parts();

        let lightd_info = LightdInfo {
            // The node's version, not this crate's: `CARGO_PKG_VERSION` here is
            // `zebra-rpc`, which clients would read as the node version.
            version: info.build().clone(),
            vendor: "ZcashFoundation/zebra".to_string(),
            taddr_support: true,
            chain_name: blockchain_info.chain().clone(),
            sapling_activation_height: self.network.sapling_activation_height().0.into(),
            consensus_branch_id: format!("{tip_branch_id:08x}"),
            block_height: blockchain_info.blocks().0.into(),
            git_commit: String::new(),
            branch: String::new(),
            build_date: String::new(),
            build_user: String::new(),
            estimated_height: blockchain_info.estimated_height().0.into(),
            zcashd_build: info.build().clone(),
            zcashd_subversion: info.subversion().clone(),
            donation_address: String::new(),
        };

        Ok(Response::new(lightd_info))
    }

    async fn ping(&self, _request: Request<Duration>) -> Result<Response<PingResponse>, Status> {
        // The `Ping` method is a testing-only method that can be used for denial of
        // service, so it is disabled, like in production lightwalletd instances.
        Err(Status::unimplemented("Ping is disabled"))
    }
}

/// Converts a [`BlockId`] into a [`HashOrHeight`], using the hash if it is present
/// and the height otherwise.
fn block_id_to_hash_or_height(block_id: BlockId) -> Result<HashOrHeight, Status> {
    if !block_id.hash.is_empty() {
        // Hashes are sent in canonical little-endian (internal) byte order, per
        // zcash/lightwalletd#512. `GetTreeState` in lightwalletd still reads
        // big-endian here, which is an unfixed instance of the same bug.
        let hash: [u8; 32] = block_id.hash.try_into().map_err(|hash: Vec<u8>| {
            Status::invalid_argument(format!("block hash must be 32 bytes, got {}", hash.len()))
        })?;

        Ok(HashOrHeight::Hash(block::Hash(hash)))
    } else if block_id.height == 0 {
        // Neither field is set. Reading that as a request for genesis, as lightwalletd
        // does not, would answer an uninitialised request with a block that has no
        // shielded data — so the client's scan "succeeds" and finds nothing.
        Err(Status::invalid_argument(
            "block id must specify a hash or a height",
        ))
    } else {
        let height = u32::try_from(block_id.height)
            .ok()
            .and_then(|height| block::Height::try_from(height).ok())
            .ok_or_else(|| {
                Status::invalid_argument(format!("block height out of range: {}", block_id.height))
            })?;

        Ok(HashOrHeight::Height(height))
    }
}

/// Fetches a block and its note commitment tree sizes from the state, and converts
/// them into a [`CompactBlock`].
async fn compact_block<ReadStateService: ReadState>(
    read_state: ReadStateService,
    hash_or_height: HashOrHeight,
    nullifiers_only: bool,
) -> Result<CompactBlock, Status> {
    let block = match read_state
        .clone()
        .oneshot(ReadRequest::Block(hash_or_height))
        .await
    {
        Ok(ReadResponse::Block(Some(block))) => block,
        Ok(ReadResponse::Block(None)) => return Err(Status::not_found("block not found")),
        Ok(_) => unreachable!("unexpected response type from ReadStateService"),
        Err(error) => {
            return Err(Status::internal(format!("failed to read block: {error}")));
        }
    };

    let hash = block.hash();

    // `nullifiers_only` responses carry zeroed tree sizes, like lightwalletd's
    // `pruneCompactBlockToNullifiers`, so don't read the trees at all.
    let (sapling_tree_size, orchard_tree_size) = if nullifiers_only {
        (0, 0)
    } else {
        // Fetch the trees by the block's hash rather than the caller's hash or height,
        // so a concurrent reorg can't make the tree sizes inconsistent with the block.
        let sapling_request = read_state
            .clone()
            .oneshot(ReadRequest::SaplingTree(hash.into()));
        let orchard_request = read_state.oneshot(ReadRequest::OrchardTree(hash.into()));

        let (sapling_response, orchard_response) = futures::join!(sapling_request, orchard_request);

        // A missing tree is not an empty tree: serving zero would tell the client no
        // notes exist as of this block, corrupting every position derived from it.
        let sapling_tree_size = match sapling_response {
            Ok(ReadResponse::SaplingTree(Some(tree))) => tree.count(),
            Ok(ReadResponse::SaplingTree(None)) => {
                return Err(Status::aborted(
                    "sapling tree not found for this block, it may have been reorged",
                ));
            }
            Ok(_) => unreachable!("unexpected response type from ReadStateService"),
            Err(error) => {
                return Err(Status::internal(format!(
                    "failed to read sapling tree: {error}"
                )));
            }
        };

        let orchard_tree_size = match orchard_response {
            Ok(ReadResponse::OrchardTree(Some(tree))) => tree.count(),
            Ok(ReadResponse::OrchardTree(None)) => {
                return Err(Status::aborted(
                    "orchard tree not found for this block, it may have been reorged",
                ));
            }
            Ok(_) => unreachable!("unexpected response type from ReadStateService"),
            Err(error) => {
                return Err(Status::internal(format!(
                    "failed to read orchard tree: {error}"
                )));
            }
        };

        (sapling_tree_size, orchard_tree_size)
    };

    let height = block
        .coinbase_height()
        .ok_or_else(|| Status::internal("block in the state must have a height"))?;

    Ok(CompactBlock::from_block(
        &block,
        hash,
        height,
        sapling_tree_size,
        orchard_tree_size,
        nullifiers_only,
    ))
}

/// Returns a stream of compact blocks for the given block range.
///
/// The range is inclusive, and the blocks are streamed in the order given by the
/// range: ascending if `start <= end`, and descending otherwise.
fn block_range_stream<ReadStateService: ReadState>(
    read_state: ReadStateService,
    block_range: BlockRange,
    nullifiers_only: bool,
) -> Result<ResponseStream<CompactBlock>, Status> {
    let range_height = |block_id: Option<BlockId>, name: &str| {
        let block_id =
            block_id.ok_or_else(|| Status::invalid_argument(format!("{name} not specified")))?;
        match block_id_to_hash_or_height(block_id)? {
            HashOrHeight::Height(height) => Ok(height.0),
            HashOrHeight::Hash(_) => Err(Status::invalid_argument(format!(
                "{name} must be specified as a height"
            ))),
        }
    };

    let start = range_height(block_range.start, "start block")?;
    let end = range_height(block_range.end, "end block")?;

    let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
    let response_stream = ReceiverStream::new(response_receiver);

    tokio::spawn(async move {
        let heights: Box<dyn Iterator<Item = u32> + Send> = if start <= end {
            Box::new(start..=end)
        } else {
            Box::new((end..=start).rev())
        };

        for height in heights {
            let hash_or_height = HashOrHeight::Height(block::Height(height));
            let response = compact_block(read_state.clone(), hash_or_height, nullifiers_only).await;
            let is_err = response.is_err();

            if !send_bounded(&response_sender, response).await || is_err {
                return;
            }
        }
    });

    Ok(Box::pin(response_stream))
}

/// Returns a stream of raw transactions for the given transparent address and block
/// range, in block order.
async fn taddress_transactions_stream<Rpc, ReadStateService>(
    rpc: &Rpc,
    read_state: ReadStateService,
    request: TransparentAddressBlockFilter,
) -> Result<ResponseStream<RawTransaction>, Status>
where
    Rpc: RpcMethods,
    ReadStateService: ReadState,
{
    let range = request
        .range
        .ok_or_else(|| Status::invalid_argument("block range not specified"))?;

    // A zero start or end height means the full range in the underlying RPC method.
    let range_height = |block_id: Option<BlockId>| -> Result<Option<u32>, Status> {
        let Some(block_id) = block_id else {
            return Ok(None);
        };
        if !block_id.hash.is_empty() {
            return Err(Status::invalid_argument(
                "block range must be specified by height",
            ));
        }
        let height = u32::try_from(block_id.height).map_err(|_| {
            Status::invalid_argument(format!("block height out of range: {}", block_id.height))
        })?;
        Ok((height != 0).then_some(height))
    };

    let txids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest::new(
            vec![request.address],
            range_height(range.start)?,
            range_height(range.end)?,
        ))
        .await
        .map_err(rpc_arg_error_to_status)?;

    let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
    let response_stream = ReceiverStream::new(response_receiver);

    tokio::spawn(async move {
        for txid in txids {
            let response = match txid.parse() {
                Ok(txid) => raw_transaction_by_txid(&read_state, txid).await,
                Err(_) => Err(Status::internal(
                    "transaction ids must be valid hex strings",
                )),
            };
            let is_err = response.is_err();

            if !send_bounded(&response_sender, response).await || is_err {
                return;
            }
        }
    });

    Ok(Box::pin(response_stream))
}

/// Fetches a mined transaction from the best chain by its transaction ID.
async fn raw_transaction_by_txid<ReadStateService: ReadState>(
    read_state: &ReadStateService,
    txid: transaction::Hash,
) -> Result<RawTransaction, Status> {
    match read_state
        .clone()
        .oneshot(ReadRequest::Transaction(txid))
        .await
    {
        Ok(ReadResponse::Transaction(Some(mined_tx))) => Ok(RawTransaction {
            data: serialize_transaction(&mined_tx.tx)?,
            height: mined_tx.height.0.into(),
        }),
        Ok(ReadResponse::Transaction(None)) => Err(Status::not_found("transaction not found")),
        Ok(_) => unreachable!("unexpected response type from ReadStateService"),
        Err(error) => Err(Status::internal(format!(
            "failed to read transaction: {error}"
        ))),
    }
}

/// Serializes a transaction into raw bytes for a [`RawTransaction`].
fn serialize_transaction(tx: &transaction::Transaction) -> Result<Vec<u8>, Status> {
    tx.zcash_serialize_to_vec()
        .map_err(|_| Status::internal("failed to serialize transaction"))
}

/// Returns an error if `addresses` exceeds [`MAX_REQUEST_ADDRESSES`].
/// Collects the transparent addresses a client streams, validating each one as it
/// arrives and stopping at the first invalid or excess entry.
///
/// Each message is only bounded by the gRPC message size, so buffering unvalidated
/// input is unbounded in bytes even though the message count is capped.
async fn collect_streamed_addresses<S>(mut address_stream: S) -> Result<Vec<String>, Status>
where
    S: futures::Stream<Item = Result<Address, Status>> + Unpin,
{
    use futures::StreamExt;

    let mut addresses = Vec::new();

    while let Some(address) = address_stream.next().await.transpose()? {
        if addresses.len() >= MAX_REQUEST_ADDRESSES {
            return Err(Status::invalid_argument(format!(
                "too many addresses: the limit is {MAX_REQUEST_ADDRESSES}"
            )));
        }

        if address.address.parse::<transparent::Address>().is_err() {
            return Err(Status::invalid_argument("invalid transparent address"));
        }

        addresses.push(address.address);
    }

    Ok(addresses)
}

/// Returns an error if there are too many addresses in a single request.
fn check_address_count(addresses: &[String]) -> Result<(), Status> {
    if addresses.len() > MAX_REQUEST_ADDRESSES {
        return Err(Status::invalid_argument(format!(
            "too many addresses: the limit is {MAX_REQUEST_ADDRESSES}"
        )));
    }

    Ok(())
}

/// Fetches the total balance of a list of transparent addresses.
async fn address_balance<Rpc: RpcMethods>(
    rpc: &Rpc,
    addresses: Vec<String>,
) -> Result<Balance, Status> {
    check_address_count(&addresses)?;

    let balance = rpc
        .get_address_balance(GetAddressBalanceRequest::new(addresses))
        .await
        .map_err(rpc_arg_error_to_status)?;

    Ok(Balance {
        // Cast is safe: the total ZEC supply in zatoshis fits in an `i64`.
        value_zat: balance.balance() as i64,
    })
}

/// Fetches the UTXOs for a list of transparent addresses, filtered by the given
/// start height and maximum number of entries.
async fn address_utxos<Rpc: RpcMethods>(
    rpc: &Rpc,
    args: GetAddressUtxosArg,
) -> Result<Vec<GetAddressUtxosReply>, Status> {
    check_address_count(&args.addresses)?;

    let response = rpc
        .get_address_utxos(GetAddressUtxosRequest::new(args.addresses, false))
        .await
        .map_err(rpc_arg_error_to_status)?;

    let GetAddressUtxosResponse::Utxos(utxos) = response else {
        unreachable!("chain info is never requested");
    };

    let max_entries = if args.max_entries == 0 {
        usize::MAX
    } else {
        // Cast is safe: `usize` is at least 32 bits on all supported platforms.
        args.max_entries as usize
    };

    Ok(utxos
        .iter()
        .filter(|utxo| u64::from(utxo.height().0) >= args.start_height)
        .take(max_entries)
        .map(|utxo| GetAddressUtxosReply {
            address: utxo.address().to_string(),
            txid: utxo.txid().0.to_vec(),
            // Cast is safe: output indexes are 32-bit values, and valid indexes are
            // bounded by the maximum block size.
            index: utxo.output_index().index() as i32,
            script: utxo.script().as_raw_bytes().to_vec(),
            // Cast is safe: the total ZEC supply in zatoshis fits in an `i64`.
            value_zat: utxo.satoshis() as i64,
            height: utxo.height().0.into(),
        })
        .collect())
}

/// Sends every mempool transaction that isn't already in `sent_txids`, adding the ones
/// it sends, and returns false if the client went away or the mempool query failed.
///
/// Used for the initial snapshot, and again whenever the change subscription lags and
/// the stream would otherwise silently omit the dropped transactions.
async fn send_unseen_mempool_txs<Mempool: MempoolService>(
    mempool: &Mempool,
    response_sender: &tokio::sync::mpsc::Sender<Result<RawTransaction, Status>>,
    sent_txids: &mut HashSet<transaction::Hash>,
) -> bool {
    let transactions = match mempool_transactions(mempool.clone()).await {
        Ok(transactions) => transactions,
        Err(status) => {
            let _ = send_bounded(response_sender, Err(status)).await;
            return false;
        }
    };

    for tx in transactions {
        if sent_txids.contains(&tx.id.mined_id()) {
            continue;
        }

        if !send_raw_mempool_tx(response_sender, &tx).await {
            return false;
        }

        sent_txids.insert(tx.id.mined_id());
    }

    true
}

/// Fetches all transactions currently in the mempool.
async fn mempool_transactions<Mempool: MempoolService>(
    mempool: Mempool,
) -> Result<Vec<transaction::UnminedTx>, Status> {
    let response = mempool
        .oneshot(mempool::Request::FullTransactions)
        .await
        .map_err(|error| Status::internal(format!("failed to query mempool: {error}")))?;

    match response {
        mempool::Response::FullTransactions { transactions, .. } => {
            Ok(transactions.into_iter().map(|tx| tx.transaction).collect())
        }
        _ => unreachable!("unexpected response type from mempool service"),
    }
}

/// Returns the transaction IDs excluded by an `Exclude` list of shortened
/// transaction IDs.
///
/// Each entry is a reversed prefix of the big-endian (display order) transaction ID
/// hex — equivalently, a suffix of the little-endian transaction ID bytes sent in
/// `CompactTx.hash` — matching lightwalletd, which reverses each entry before
/// comparing it with big-endian transaction IDs.
///
/// If an entry matches more than one transaction, none of the matching transactions
/// are excluded, following the lightwalletd specification.
fn excluded_txids(txids: &[transaction::Hash], exclude: &[Vec<u8>]) -> HashSet<transaction::Hash> {
    let mut excluded = HashSet::new();

    for suffix in exclude {
        // An empty entry doesn't identify a transaction, and would otherwise
        // exclude the only transaction in a single-transaction mempool. An entry
        // longer than a transaction ID can never match one.
        if suffix.is_empty() || suffix.len() > 32 {
            continue;
        }

        let mut matches = txids.iter().filter(|txid| txid.0.ends_with(suffix));

        if let (Some(txid), None) = (matches.next(), matches.next()) {
            excluded.insert(*txid);
        }
    }

    excluded
}

/// Sends `item` to a response stream, returning false if the client disconnected or
/// stopped reading.
///
/// The send is bounded by [`STREAM_SEND_TIMEOUT`] so a client that holds its
/// connection open without consuming the stream can't park the producer task, and
/// its buffered responses, for the lifetime of the process.
async fn send_bounded<T>(
    response_sender: &tokio::sync::mpsc::Sender<Result<T, Status>>,
    item: Result<T, Status>,
) -> bool {
    matches!(
        tokio::time::timeout(STREAM_SEND_TIMEOUT, response_sender.send(item)).await,
        Ok(Ok(()))
    )
}

/// Sends a mempool transaction to a raw transaction stream, returning false if the
/// client disconnected or hung, or the transaction could not be serialized.
async fn send_raw_mempool_tx(
    response_sender: &tokio::sync::mpsc::Sender<Result<RawTransaction, Status>>,
    tx: &transaction::UnminedTx,
) -> bool {
    let Ok(data) = tx.transaction.zcash_serialize_to_vec() else {
        let _ = send_bounded(
            response_sender,
            Err(Status::internal("failed to serialize transaction")),
        )
        .await;
        return false;
    };

    // A height of 0 means the transaction is in the mempool. The protocol comment
    // says `GetMempoolStream` sends the latest block height instead, but that
    // documentation is wrong and lightwalletd always sends 0:
    // <https://github.com/zcash/librustzcash/issues/1484>
    send_bounded(response_sender, Ok(RawTransaction { data, height: 0 })).await
}

/// Converts a [`GetTreestateResponse`] into a lightwalletd [`TreeState`].
fn treestate_to_proto(network: &Network, treestate: GetTreestateResponse) -> TreeState {
    let final_state_hex =
        |commitments: &Option<Vec<u8>>| commitments.as_ref().map(hex::encode).unwrap_or_default();

    TreeState {
        network: network.bip70_network_name(),
        height: treestate.height().0.into(),
        hash: treestate.hash().to_string(),
        time: treestate.time(),
        sapling_tree: final_state_hex(treestate.sapling().commitments().final_state()),
        orchard_tree: final_state_hex(treestate.orchard().commitments().final_state()),
    }
}

/// Converts a JSON-RPC method error into a gRPC status, for a lookup of something the
/// client asked for by identifier.
fn rpc_error_to_status(error: jsonrpsee_types::ErrorObjectOwned) -> Status {
    // Missing blocks, transactions, and invalid addresses or keys are reported with
    // zcashd's `INVALID_ADDRESS_OR_KEY` (-5) and `INVALID_PARAMETER` (-8) error codes.
    match error.code() {
        -5 | -8 => Status::not_found(error.message().to_string()),
        _ => Status::internal(error.message().to_string()),
    }
}

/// Converts a JSON-RPC method error into a gRPC status, for a call whose arguments the
/// client supplied and Zebra has not already validated.
///
/// zcashd reuses the same error codes for "no such thing" and "that argument is not
/// valid", so only the caller knows which it meant. Reporting a malformed address as
/// `NOT_FOUND`, or a bad range as `INTERNAL`, blames the server for the client's input.
fn rpc_arg_error_to_status(error: jsonrpsee_types::ErrorObjectOwned) -> Status {
    // -5 `INVALID_ADDRESS_OR_KEY`, -8 `INVALID_PARAMETER`, -32602 `InvalidParams`.
    match error.code() {
        -5 | -8 | -32602 => Status::invalid_argument(error.message().to_string()),
        _ => Status::internal(error.message().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::Arc;
    use tonic::Code;
    use zebra_chain::serialization::BytesInDisplayOrder;

    /// A streamed address list must be validated entry by entry, so an invalid
    /// address ends the stream instead of being buffered until it closes.
    #[tokio::test]
    async fn streamed_addresses_are_validated_before_the_stream_ends() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _init_guard = zebra_test::init();

        let valid = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd".to_string();
        let yielded = Arc::new(AtomicUsize::new(0));

        // A valid address, an invalid one, then more input the server must not read.
        let addresses = futures::stream::iter(
            [valid.clone(), "not an address".to_string(), valid.clone()]
                .map(|address| Ok(Address { address })),
        )
        .inspect({
            let yielded = yielded.clone();
            move |_| {
                yielded.fetch_add(1, Ordering::SeqCst);
            }
        });

        let status = collect_streamed_addresses(Box::pin(addresses))
            .await
            .expect_err("an invalid address must be rejected");

        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(
            yielded.load(Ordering::SeqCst),
            2,
            "the server must stop reading at the first invalid address",
        );
    }

    /// The streamed address count is bounded, so a client can't buffer without limit.
    #[tokio::test]
    async fn streamed_addresses_are_bounded() {
        let _init_guard = zebra_test::init();

        let valid = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd".to_string();
        let addresses = futures::stream::iter(
            std::iter::repeat_n(valid, MAX_REQUEST_ADDRESSES + 1)
                .map(|address| Ok(Address { address })),
        );

        let status = collect_streamed_addresses(Box::pin(addresses))
            .await
            .expect_err("too many addresses must be rejected");

        assert_eq!(status.code(), Code::InvalidArgument);
    }

    fn txid(byte: u8) -> transaction::Hash {
        transaction::Hash::from_bytes_in_display_order(&[byte; 32])
    }

    #[test]
    fn block_id_hash_round_trips_internal_byte_order() {
        let hash = block::Hash([7; 32]);
        let block_id = BlockId {
            height: 0,
            hash: hash.0.to_vec(),
        };

        let hash_or_height =
            block_id_to_hash_or_height(block_id).expect("a 32-byte hash should convert");

        assert_eq!(hash_or_height, HashOrHeight::Hash(hash));
    }

    #[test]
    fn block_id_height_is_used_when_hash_is_empty() {
        let block_id = BlockId {
            height: 500_000,
            hash: Vec::new(),
        };

        let hash_or_height =
            block_id_to_hash_or_height(block_id).expect("a valid height should convert");

        assert_eq!(hash_or_height, HashOrHeight::Height(block::Height(500_000)));
    }

    #[test]
    fn block_id_rejects_wrong_hash_length_and_out_of_range_height() {
        let status = block_id_to_hash_or_height(BlockId {
            height: 0,
            hash: vec![0; 31],
        })
        .expect_err("a 31-byte hash should be rejected");
        assert_eq!(status.code(), Code::InvalidArgument);

        let status = block_id_to_hash_or_height(BlockId {
            height: u64::MAX,
            hash: Vec::new(),
        })
        .expect_err("an out-of-range height should be rejected");
        assert_eq!(status.code(), Code::InvalidArgument);
    }

    #[test]
    fn exclude_list_matches_reversed_display_order_prefixes() {
        // A txid whose internal (little-endian) bytes are 0, 1, ..., 31,
        // so its display order is 31, 30, ..., 0.
        let asymmetric = transaction::Hash(core::array::from_fn(|i| i as u8));
        let txids = [txid(1), txid(2), asymmetric];

        // A shortened txid is a reversed display-order prefix: the client shortens
        // the display hex "1f1e1d..." to "1f1e", and sends it reversed.
        let excluded = excluded_txids(&txids, &[vec![30, 31]]);
        assert_eq!(excluded, [asymmetric].into_iter().collect());

        // A display-order (unreversed) prefix must not match.
        assert!(excluded_txids(&txids, &[vec![31, 30]]).is_empty());

        // A full txid in `CompactTx.hash` (internal) order excludes its match.
        let excluded = excluded_txids(&txids, &[asymmetric.0.to_vec()]);
        assert_eq!(excluded, [asymmetric].into_iter().collect());
    }

    #[test]
    fn exclude_list_keeps_transactions_with_ambiguous_or_unknown_entries() {
        let txids = [txid(1), txid(2)];

        // An empty entry doesn't identify a transaction, so nothing is excluded,
        // even when it is the only transaction.
        assert!(excluded_txids(&txids[..1], &[Vec::new()]).is_empty());

        // An entry that matches nothing excludes nothing.
        assert!(excluded_txids(&txids, &[vec![9, 9, 9]]).is_empty());

        // An entry that matches more than one transaction excludes nothing.
        let txids = [txid(7), txid(7)];
        assert!(excluded_txids(&txids, &[vec![7, 7]]).is_empty());
    }
}
