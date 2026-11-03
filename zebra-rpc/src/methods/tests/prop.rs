//! Randomised property tests for RPC methods.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt::Debug,
    sync::Arc,
};

use futures::{join, FutureExt, TryFutureExt};
use hex::{FromHex, ToHex};
use jsonrpsee_types::{ErrorCode, ErrorObject};
use proptest::{collection::vec, prelude::*};
use thiserror::Error;
use tokio::sync::oneshot;
use tower::buffer::Buffer;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, Height},
    chain_sync_status::MockSyncStatus,
    chain_tip::{mock::MockChainTip, ChainTip, NoChainTip},
    history_tree::HistoryTree,
    parameters::{ConsensusBranchId, Network, NetworkUpgrade},
    serialization::{DateTime32, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction::{self, Transaction, UnminedTx, VerifiedUnminedTx},
    transparent,
    value_balance::ValueBalance,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::mempool;
use zebra_state::{BlockQueryFields, BoxError, GetBlockTemplateChainInfo};
use zebra_test::mock_service::MockService;

use crate::methods::{
    self,
    get_block::{self, GetBlockPlan, TxShape},
    types::{
        get_block::{GetBlockField, GetBlockQuery},
        get_blockchain_info::GetBlockchainInfoBalance,
        get_raw_mempool::{GetRawMempoolResponse, MempoolObject},
    },
};

use super::super::{
    GetAddressBalanceRequest, GetAddressBalanceResponse, NetworkUpgradeStatus, RpcImpl, RpcServer,
    SendRawTransactionResponse,
};

proptest! {
    /// Test that when sending a raw transaction, it is received by the mempool service.
    #[test]
    fn mempool_receives_raw_tx(transaction in any::<Transaction>(), network in any::<Network>()) {
        prop_assume!(!(transaction.is_coinbase() && transaction.sapling_spends().count() > 0));

        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let hash = SendRawTransactionResponse(transaction.hash());

            let transaction_bytes = transaction.zcash_serialize_to_vec()?;

            let transaction_hex = hex::encode(&transaction_bytes);

            let send_task = tokio::spawn(async move { rpc.send_raw_transaction(transaction_hex, None).await });

            let unmined_transaction = UnminedTx::from(std::sync::Arc::new(transaction));
            let expected_request = mempool::Request::Queue(vec![unmined_transaction.into()]);
            let (rsp_tx, rsp_rx) = oneshot::channel();
            let _ = rsp_tx.send(Ok(()));
            let response = mempool::Response::Queued(vec![Ok(rsp_rx)]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            state.expect_no_requests().await?;

            let result = send_task.await.expect("send_raw_transaction should not panic");

            prop_assert_eq!(result, Ok(hash));

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test that mempool errors are forwarded to the caller.
    ///
    /// Mempool service errors should become server errors.
    #[test]
    fn mempool_errors_are_forwarded(transaction in any::<Transaction>(), network in any::<Network>()) {
        prop_assume!(!(transaction.is_coinbase() && transaction.sapling_spends().count() > 0));

        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let transaction_bytes = transaction.zcash_serialize_to_vec()?;
            let transaction_hex = hex::encode(&transaction_bytes);

            let _rpc = rpc.clone();
            let _transaction_hex = transaction_hex.clone();
            let send_task = tokio::spawn(async move { _rpc.send_raw_transaction(_transaction_hex, None).await });

            let unmined_transaction = UnminedTx::from(std::sync::Arc::new(transaction));
            let expected_request = mempool::Request::Queue(vec![unmined_transaction.clone().into()]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(Err(DummyError));

            state.expect_no_requests().await?;

            let result = send_task.await.expect("send_raw_transaction should not panic");

            check_err_code(result, ErrorCode::ServerError(-1))?;

            let send_task = tokio::spawn(async move { rpc.send_raw_transaction(transaction_hex.clone(), None).await });

            let expected_request = mempool::Request::Queue(vec![unmined_transaction.clone().into()]);

            let (rsp_tx, rsp_rx) = oneshot::channel();
            let _ = rsp_tx.send(Err("any verification error".into()));
            mempool
                .expect_request(expected_request)
                .await?
                .respond(Ok::<_, BoxError>(mempool::Response::Queued(vec![Ok(rsp_rx)])));

            let result = send_task.await.expect("send_raw_transaction should not panic");

            check_err_code(result, ErrorCode::ServerError(-25))?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test that when the mempool rejects a transaction the caller receives an error.
    #[test]
    fn rejected_txs_are_reported(transaction in any::<Transaction>(), network in any::<Network>()) {
        prop_assume!(!(transaction.is_coinbase() && transaction.sapling_spends().count() > 0));

        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let tx = hex::encode(&transaction.zcash_serialize_to_vec()?);
            let req = mempool::Request::Queue(vec![UnminedTx::from(std::sync::Arc::new(transaction)).into()]);
            let rsp = mempool::Response::Queued(vec![Err(DummyError.into())]);
            let mempool_query = mempool.expect_request(req).map_ok(|r| r.respond(rsp));

            let (rpc_rsp, _) = tokio::join!(rpc.send_raw_transaction(tx, None), mempool_query);

            check_err_code(rpc_rsp, ErrorCode::ServerError(-1))?;

            // Check that no state request was made.
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test that the method rejects non-hexadecimal characters.
    ///
    /// Try to call `send_raw_transaction` using a string parameter that has at least one
    /// non-hexadecimal character, and check that it fails with an expected error.
    #[test]
    fn non_hex_string_is_error(non_hex_string in ".*[^0-9A-Fa-f].*", network in any::<Network>()) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let send_task = rpc.send_raw_transaction(non_hex_string, None);

            // Check that there are no further requests.
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            check_err_code(send_task.await, ErrorCode::ServerError(-22))?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test that the method rejects an input that's not a transaction.
    ///
    /// Try to call `send_raw_transaction` using random bytes that fail to deserialize as a
    /// transaction, and check that it fails with an expected error.
    #[test]
    fn invalid_tx_results_in_an_error(random_bytes in any::<Vec<u8>>(), network in any::<Network>()) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        prop_assume!(Transaction::zcash_deserialize(&*random_bytes).is_err());

        runtime.block_on(async move {
            let send_task = rpc.send_raw_transaction(hex::encode(random_bytes), None);

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            check_err_code(send_task.await, ErrorCode::ServerError(-22))?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test that the `getrawmempool` method forwards the transactions in the mempool.
    ///
    /// Make the mock mempool service return a list of transaction IDs, and check that the RPC call
    /// returns those IDs as hexadecimal strings.
    #[test]
    fn mempool_transactions_are_sent_to_caller(transactions in any::<Vec<VerifiedUnminedTx>>(),
                                               network in any::<Network>(),
                                               verbose in any::<Option<bool>>()) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            if verbose.unwrap_or(false) {
                let (expected_response, mempool_query) = {
                    let transactions_by_id = transactions
                        .iter()
                        .map(|unmined_tx| {(unmined_tx.transaction.id.mined_id(), unmined_tx)})
                        .collect::<HashMap<_, _>>();
                    let transaction_dependencies = Default::default();
                    let txs = transactions
                        .iter()
                        .map(|unmined_tx| {
                            (
                                unmined_tx.transaction.id.mined_id().encode_hex(),
                                MempoolObject::from_verified_unmined_tx(
                                    unmined_tx,
                                    &transactions_by_id,
                                    &transaction_dependencies,
                                ),
                            )
                        })
                        .collect::<HashMap<_, _>>();

                    let mempool_query = mempool.expect_request(mempool::Request::FullTransactions)
                        .map_ok(|r| r.respond(mempool::Response::FullTransactions {
                            transactions,
                            transaction_dependencies,
                            last_seen_tip_hash: [0; 32].into(),
                        }));

                    (GetRawMempoolResponse::Verbose(txs), mempool_query)
                };

                let (rpc_rsp, _) = tokio::join!(rpc.get_raw_mempool(verbose), mempool_query);
                prop_assert_eq!(rpc_rsp?, expected_response);
            } else {
                let (expected_rsp, mempool_query) = {
                    let mut tx_ids = transactions
                        .iter()
                        .map(|tx| tx.transaction.id.mined_id().encode_hex::<String>())
                        .collect::<Vec<_>>();

                    tx_ids.sort();

                    let mempool_rsp = transactions
                        .iter()
                        .map(|tx| tx.transaction.id)
                        .collect::<HashSet<_>>();

                    let mempool_query = mempool.expect_request(mempool::Request::TransactionIds)
                            .map_ok(|r| r.respond(mempool::Response::TransactionIds(mempool_rsp)));

                    (GetRawMempoolResponse::TxIds(tx_ids), mempool_query,)
                };

                let (rpc_rsp, _) = tokio::join!(rpc.get_raw_mempool(verbose), mempool_query);
                prop_assert_eq!(rpc_rsp?, expected_rsp);
            }

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Calls `get_raw_transaction` with:
    ///
    /// 1. an invalid TXID that won't deserialize;
    /// 2. a valid TXID that is not in the mempool nor in the state;
    ///
    /// and checks that the RPC returns the right error code.
    #[test]
    fn check_err_for_get_raw_transaction(unknown_txid: transaction::Hash,
                                         invalid_txid in invalid_txid(),
                                         network: Network) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            // Check the invalid TXID first.
            let rpc_rsp = rpc.get_raw_transaction(invalid_txid, Some(1), None).await;

            check_err_code(rpc_rsp, ErrorCode::ServerError(-5))?;

            // Check that no further requests were made.
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // Now check the unknown TXID.
            let mempool_query = mempool
                .expect_request(mempool::Request::TransactionsByMinedId([unknown_txid].into()))
                .map_ok(|r| r.respond(mempool::Response::Transactions(vec![])));

            let state_query = state
                .expect_request(zebra_state::ReadRequest::AnyChainTransaction(unknown_txid))
                .map_ok(|r| r.respond(zebra_state::ReadResponse::AnyChainTransaction(None)));

            let rpc_query = rpc.get_raw_transaction(unknown_txid.encode_hex(), Some(1), None);

            let (rpc_rsp, _, _) =  tokio::join!(rpc_query, mempool_query, state_query);

            check_err_code(rpc_rsp, ErrorCode::ServerError(-5))?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test the `get_blockchain_info` response when Zebra's state is empty.
    #[test]
    fn get_blockchain_info_response_without_a_chain_tip(network in any::<Network>()) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network.clone(), NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        let genesis_hash = network.genesis_hash();

        runtime.block_on(async move {
            let response_fut = rpc.get_blockchain_info();
            let mock_state_handler = {
                let mut state = state.clone();
                async move {
                    state
                        .expect_request(zebra_state::ReadRequest::UsageInfo)
                        .await
                        .expect("getblockchaininfo should call mock state service with correct request")
                        .respond(zebra_state::ReadResponse::UsageInfo(0));

                    state
                        .expect_request(zebra_state::ReadRequest::TipPoolValues)
                        .await
                        .expect("getblockchaininfo should call mock state service with correct request")
                        .respond(Err(BoxError::from("no chain tip available yet")));

                    state
                        .expect_request(zebra_state::ReadRequest::ChainInfo)
                        .await
                        .expect("getblockchaininfo should call mock state service with correct request")
                        .respond(zebra_state::ReadResponse::ChainInfo(GetBlockTemplateChainInfo {
                            tip_hash: genesis_hash,
                            tip_height: Height::MIN,
                            chain_history_root: HistoryTree::default().hash(),
                            expected_difficulty: Default::default(),
                            cur_time: DateTime32::now(),
                            min_time: DateTime32::now(),
                            max_time: DateTime32::now(),
                            chain_value_pools: Default::default(),
                        }));

                    state
                        .expect_request(zebra_state::ReadRequest::HistoryCapabilities)
                        .await
                        .expect("history is requested after the existing chain info requests")
                        .respond(zebra_state::ReadResponse::HistoryCapabilities(Default::default()));
                }
            };

            let (response, _) = tokio::join!(response_fut, mock_state_handler);

            prop_assert_eq!(
                response.unwrap().best_block_hash,
                genesis_hash
            );

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test the `get_blockchain_info` response using an arbitrary block as the `ChainTip`.
    #[test]
    fn get_blockchain_info_response_with_an_arbitrary_chain_tip(
        network in any::<Network>(),
        block in any::<Block>(),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) =
            mock_services(network.clone(), NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        // get arbitrary chain tip data
        let block_height = block.coinbase_height().unwrap();
        let block_hash = block.hash();
        let expected_size_on_disk = 1_000;

        // check no requests were made during this test
        runtime.block_on(async move {
            let response_fut = rpc.get_blockchain_info();
            let mock_state_handler = {
                let mut state = state.clone();
                async move {
                    state
                        .expect_request(zebra_state::ReadRequest::UsageInfo)
                        .await
                        .expect("getblockchaininfo should call mock state service with correct request")
                        .respond(zebra_state::ReadResponse::UsageInfo(expected_size_on_disk));

                    state
                        .expect_request(zebra_state::ReadRequest::TipPoolValues)
                        .await
                        .expect("getblockchaininfo should call mock state service with correct request")
                        .respond(zebra_state::ReadResponse::TipPoolValues {
                            tip_height: block_height,
                            tip_hash: block_hash,
                            value_balance: ValueBalance::default(),
                        });

                    state
                        .expect_request(zebra_state::ReadRequest::ChainInfo)
                        .await
                        .expect("getblockchaininfo should call mock state service with correct request")
                        .respond(zebra_state::ReadResponse::ChainInfo(GetBlockTemplateChainInfo {
                            tip_hash: block_hash,
                            tip_height: block_height,
                            chain_history_root: HistoryTree::default().hash(),
                            expected_difficulty: Default::default(),
                            cur_time: DateTime32::now(),
                            min_time: DateTime32::now(),
                            max_time: DateTime32::now(),
                            chain_value_pools: Default::default(),
                        }));

                    state
                        .expect_request(zebra_state::ReadRequest::HistoryCapabilities)
                        .await
                        .expect("history is requested after the existing chain info requests")
                        .respond(zebra_state::ReadResponse::HistoryCapabilities(Default::default()));
                }
            };

            let (response, _) = tokio::join!(response_fut, mock_state_handler);

            // Check response
            match response {
                Ok(info) => {
                    prop_assert_eq!(info.chain, network.bip70_network_name());
                    prop_assert_eq!(info.blocks, block_height);
                    prop_assert_eq!(info.best_block_hash, block_hash);
                    prop_assert_eq!(info.size_on_disk, expected_size_on_disk);
                    prop_assert!(info.estimated_height < Height::MAX);
                    prop_assert!(!info.pruned);
                    prop_assert_eq!(info.prune_height, None);

                    prop_assert_eq!(
                        info.consensus.chain_tip.0,
                        NetworkUpgrade::current(&network, block_height)
                            .branch_id()
                            .unwrap()
                    );
                    prop_assert_eq!(
                        info.consensus.next_block.0,
                        NetworkUpgrade::current(&network, (block_height + 1).unwrap())
                            .branch_id()
                            .unwrap()
                    );

                    for u in info.upgrades {
                        let mut status = NetworkUpgradeStatus::Active;
                        if block_height < u.1.activation_height {
                            status = NetworkUpgradeStatus::Pending;
                        }
                        prop_assert_eq!(u.1.status, status);
                    }
                }
                Err(err) => {
                    unreachable!("Test should never error with the data we are feeding it: {err}")
                }
            };

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test the `get_blockchain_info` response when tip_pool request fails.
    #[test]
    fn get_blockchain_info_returns_genesis_when_tip_pool_fails(network in any::<Network>()) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network.clone(), NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        let genesis_block = match network {
            Network::Mainnet => {
                let block_bytes = &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES;
                let block: Arc<Block> = block_bytes.zcash_deserialize_into().expect("block is valid");
                block
            },
            Network::Testnet(_) => {
                let block_bytes = &zebra_test::vectors::BLOCK_TESTNET_GENESIS_BYTES;
                let block: Arc<Block> = block_bytes.zcash_deserialize_into().expect("block is valid");
                block
            },
        };

        // Genesis block fields
        let expected_size_on_disk = 1_000;

        runtime.block_on(async move {
            let response_fut = rpc.get_blockchain_info();
            let mock_state_handler = {
                let mut state = state.clone();
                async move {
                state
                    .expect_request(zebra_state::ReadRequest::UsageInfo)
                    .await
                    .expect("getblockchaininfo should call mock state service with correct request")
                    .respond(zebra_state::ReadResponse::UsageInfo(expected_size_on_disk));

                state.expect_request(zebra_state::ReadRequest::TipPoolValues)
                    .await
                    .expect("getblockchaininfo should call mock state service with correct request")
                    .respond(Err(BoxError::from("tip values not available")));

                state
                    .expect_request(zebra_state::ReadRequest::ChainInfo)
                    .await
                    .expect("getblockchaininfo should call mock state service with correct request")
                    .respond(Err(BoxError::from("chain info not available")));

                    state
                        .expect_request(zebra_state::ReadRequest::HistoryCapabilities)
                        .await
                        .expect("history is requested after the existing chain info requests")
                        .respond(zebra_state::ReadResponse::HistoryCapabilities(Default::default()));
                }
            };

            let (response, _) = tokio::join!(response_fut, mock_state_handler);

            let response = response.expect("should succeed with genesis block info");

            prop_assert_eq!(response.best_block_hash, genesis_block.header.hash());
            prop_assert_eq!(response.chain, network.bip70_network_name());
            prop_assert_eq!(response.blocks, Height::MIN);
            prop_assert_eq!(response.value_pools, GetBlockchainInfoBalance::value_pools(ValueBalance::zero(), None));

            let genesis_branch_id = NetworkUpgrade::current(&network, Height::MIN).branch_id().unwrap_or(ConsensusBranchId::RPC_MISSING_ID);
            let next_height = (Height::MIN + 1).expect("genesis height plus one is next height and valid");
            let next_branch_id = NetworkUpgrade::current(&network, next_height).branch_id().unwrap_or(ConsensusBranchId::RPC_MISSING_ID);

            prop_assert_eq!(response.consensus.chain_tip.0, genesis_branch_id);
            prop_assert_eq!(response.consensus.next_block.0, next_branch_id);

            for (_, upgrade_info) in response.upgrades {
                let status = if Height::MIN < upgrade_info.activation_height {
                    NetworkUpgradeStatus::Pending
                } else {
                    NetworkUpgradeStatus::Active
                };
                prop_assert_eq!(upgrade_info.status, status);
            }

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test the `get_address_balance` RPC using an arbitrary set of addresses.
    #[test]
    fn queries_balance_for_valid_addresses(
        network in any::<Network>(),
        addresses in any::<HashSet<transparent::Address>>(),
        balance in any::<Amount<NonNegative>>(),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (chain_tip, _mock_chain_tip_sender) = MockChainTip::new();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, chain_tip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        // Prepare the list of addresses.
        let address_strings = GetAddressBalanceRequest {
            addresses: addresses
                .iter()
                .map(|address| address.to_string())
                .collect(),
        };

        // Start RPC with the mocked `ChainTip`
        runtime.block_on(async move {
            // Build the future to call the RPC
            let call = rpc.get_address_balance(address_strings);

            // The RPC should perform a state query
            let state_query = state
                .expect_request(zebra_state::ReadRequest::AddressBalance(addresses))
                .map_ok(|responder| {
                    responder.respond(zebra_state::ReadResponse::AddressBalance { balance, received: balance.into() })
                });

            // Await the RPC call and the state query
            let (response, state_query_result) = join!(call, state_query);

            state_query_result?;

            // Check that response contains the expected balance
            let received_balance = response?;

            prop_assert_eq!(received_balance, GetAddressBalanceResponse { balance: balance.into(), received: balance.into() });

            // Check no further requests were made during this test
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test the `get_address_balance` RPC using an invalid list of addresses.
    ///
    /// An error should be returned.
    #[test]
    fn does_not_query_balance_for_invalid_addresses(
        network in any::<Network>(),
        at_least_one_invalid_address in vec(".*", 1..10),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (chain_tip, _mock_chain_tip_sender) = MockChainTip::new();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, chain_tip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        prop_assume!(at_least_one_invalid_address
            .iter()
            .any(|string| string.parse::<transparent::Address>().is_err()));

        runtime.block_on(async move {

            let address_strings = GetAddressBalanceRequest {
                addresses: at_least_one_invalid_address,
            };

            // Build the future to call the RPC
            let result = rpc.get_address_balance(address_strings).await;

            check_err_code(result, ErrorCode::ServerError(-5))?;

            // Check no requests were made during this test
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test the queue functionality using `send_raw_transaction`
    #[test]
    fn rpc_queue_main_loop(tx in any::<Transaction>(), network in any::<Network>()) {
        prop_assume!(!(tx.is_coinbase() && tx.sapling_spends().count() > 0));

        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let transaction_hash = tx.hash();
            let tx_bytes = tx.zcash_serialize_to_vec()?;
            let tx_hex = hex::encode(&tx_bytes);
            let send_task = {
                let rpc = rpc.clone();
                tokio::task::spawn(async move { rpc.send_raw_transaction(tx_hex, None).await })
            };
            let tx_unmined = UnminedTx::from(std::sync::Arc::new(tx));
            let expected_request = mempool::Request::Queue(vec![tx_unmined.clone().into()]);

            // fail the mempool insertion
            mempool
                .expect_request(expected_request)
                .await
                .unwrap()
                .respond(Err(DummyError));

            let _ = send_task.await?;

            // advance enough time to have a new runner iteration
            let spacing = chrono::Duration::seconds(150);
            tokio::time::advance(spacing.to_std().unwrap()).await;

            // the runner will made a new call to TransactionsById
            let mut transactions_hash_set = HashSet::new();
            transactions_hash_set.insert(tx_unmined.id);
            let expected_request = mempool::Request::TransactionsById(transactions_hash_set);
            let response = mempool::Response::Transactions(vec![]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            // the runner will also query the state again for the transaction
            let expected_request = zebra_state::ReadRequest::KnownTransaction(transaction_hash);
            let response = zebra_state::ReadResponse::KnownTransaction(false);

            state
                .expect_request(expected_request)
                .await?
                .respond(response);

            // now a retry will be sent to the mempool
            let expected_request =
                mempool::Request::Queue(vec![mempool::Gossip::Tx(tx_unmined.clone())]);
            let (rsp_tx, rsp_rx) = oneshot::channel();
            let _ = rsp_tx.send(Ok(()));
            let response = mempool::Response::Queued(vec![Ok(rsp_rx)]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            // no more requests are done
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }

    /// Test we receive all transactions that are sent in a channel
    #[test]
    fn rpc_queue_receives_all_txs_from_channel(txs in any::<[Transaction; 2]>(),
                                               network in any::<Network>()) {
        prop_assume!(txs.iter().all(|tx| !(tx.is_coinbase() && tx.sapling_spends().count() > 0)));
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let mut transactions_hash_set = HashSet::new();
            for tx in txs.clone() {
                let rpc_clone = rpc.clone();
                // send a transaction
                let tx_bytes = tx.zcash_serialize_to_vec()?;
                let tx_hex = hex::encode(&tx_bytes);
                let send_task = tokio::task::spawn(async move { rpc_clone.send_raw_transaction(tx_hex, None).await });

                let tx_unmined = UnminedTx::from(std::sync::Arc::new(tx.clone()));
                let expected_request = mempool::Request::Queue(vec![tx_unmined.clone().into()]);

                // insert to hs we will use later
                transactions_hash_set.insert(tx_unmined.id);

                // fail the mempool insertion
                mempool
                    .clone()
                    .expect_request(expected_request)
                    .await
                    .unwrap()
                    .respond(Err(DummyError));

                let _ = send_task.await?;
            }

            // advance enough time to have a new runner iteration
            let spacing = chrono::Duration::seconds(150);
            tokio::time::advance(spacing.to_std().unwrap()).await;

            // the runner will made a new call to TransactionsById querying with both transactions
            let expected_request = mempool::Request::TransactionsById(transactions_hash_set);
            let response = mempool::Response::Transactions(vec![]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            // the runner will also query the state again for each transaction
            for _tx in txs.clone() {
                let response = zebra_state::ReadResponse::KnownTransaction(false);

                // we use `expect_request_that` because we can't guarantee the state request order
                state
                    .expect_request_that(|request| {
                        matches!(request, zebra_state::ReadRequest::KnownTransaction(_))
                    })
                    .await?
                    .respond(response);
            }

            // each transaction will be retried
            for tx in txs.clone() {
                let expected_request =
                    mempool::Request::Queue(vec![mempool::Gossip::Tx(UnminedTx::from(std::sync::Arc::new(tx)))]);
                let (rsp_tx, rsp_rx) = oneshot::channel();
                let _ = rsp_tx.send(Ok(()));
                let response = mempool::Response::Queued(vec![Ok(rsp_rx)]);

                mempool
                    .expect_request(expected_request)
                    .await?
                    .respond(response);
            }

            // no more requests are done
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            prop_assert!(mempool_tx_queue.now_or_never().is_none());

            Ok(())
        })?;
    }
}

#[derive(Clone, Copy, Debug, Error)]
#[error("a dummy error type")]
pub struct DummyError;

// Helper functions

/// Creates [`String`]s that won't deserialize into [`transaction::Hash`].
fn invalid_txid() -> BoxedStrategy<String> {
    any::<String>()
        .prop_filter("string must not deserialize into TXID", |s| {
            transaction::Hash::from_hex(s).is_err()
        })
        .boxed()
}

/// Checks that the given RPC response contains the given error code.
fn check_err_code<T>(
    rsp: Result<T, ErrorObject>,
    error_code: ErrorCode,
) -> Result<(), TestCaseError> {
    match rsp {
        Err(e) => {
            prop_assert!(
                e.code() == error_code.code(),
                "the RPC response must match the error code: {:?}",
                error_code.code()
            );
        }
        Ok(_) => {
            prop_assert!(false, "expected an error response, but got Ok");
        }
    }

    Ok(())
}

/// Creates mocked:
///
/// 1. mempool service,
/// 2. state service,
/// 3. rpc service,
///
/// and a handle to the mempool tx queue.
fn mock_services<Tip>(
    network: Network,
    chain_tip: Tip,
) -> (
    zebra_test::mock_service::MockService<
        zebra_node_services::mempool::Request,
        zebra_node_services::mempool::Response,
        zebra_test::mock_service::PropTestAssertion,
    >,
    zebra_test::mock_service::MockService<
        zebra_state::ReadRequest,
        zebra_state::ReadResponse,
        zebra_test::mock_service::PropTestAssertion,
    >,
    methods::RpcImpl<
        zebra_test::mock_service::MockService<
            zebra_node_services::mempool::Request,
            zebra_node_services::mempool::Response,
            zebra_test::mock_service::PropTestAssertion,
        >,
        tower::buffer::Buffer<
            zebra_test::mock_service::MockService<
                zebra_state::Request,
                zebra_state::Response,
                zebra_test::mock_service::PropTestAssertion,
            >,
            zebra_state::Request,
        >,
        tower::buffer::Buffer<
            zebra_test::mock_service::MockService<
                zebra_state::ReadRequest,
                zebra_state::ReadResponse,
                zebra_test::mock_service::PropTestAssertion,
            >,
            zebra_state::ReadRequest,
        >,
        Tip,
        MockAddressBookPeers,
        zebra_test::mock_service::MockService<
            zebra_consensus::Request,
            block::Hash,
            zebra_test::mock_service::PropTestAssertion,
        >,
        MockSyncStatus,
    >,
    tokio::task::JoinHandle<()>,
)
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    let mempool = MockService::build().for_prop_tests();
    let state = MockService::build().for_prop_tests();
    let read_state = MockService::build().for_prop_tests();
    let block_verifier_router = MockService::build().for_prop_tests();

    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, mempool_tx_queue) = RpcImpl::new(
        network,
        Default::default(),
        Default::default(),
        "0.0.1",
        "RPC test",
        mempool.clone(),
        Buffer::new(state.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        block_verifier_router,
        MockSyncStatus::default(),
        chain_tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    (mempool, read_state, rpc, mempool_tx_queue)
}

/// Every `getblock` field except the three `tx` shapes.
const NON_TX_FIELDS: [GetBlockField; 22] = [
    GetBlockField::Hash,
    GetBlockField::Confirmations,
    GetBlockField::Size,
    GetBlockField::Height,
    GetBlockField::Version,
    GetBlockField::MerkleRoot,
    GetBlockField::BlockCommitments,
    GetBlockField::AuthDataRoot,
    GetBlockField::FinalSaplingRoot,
    GetBlockField::FinalOrchardRoot,
    GetBlockField::TxCount,
    GetBlockField::Time,
    GetBlockField::Nonce,
    GetBlockField::Solution,
    GetBlockField::Bits,
    GetBlockField::Difficulty,
    GetBlockField::ChainSupply,
    GetBlockField::ValuePools,
    GetBlockField::Trees,
    GetBlockField::PreviousBlockHash,
    GetBlockField::NextBlockHash,
    GetBlockField::Hex,
];

/// The three `tx` shapes.
const TX_SHAPE_FIELDS: [GetBlockField; 3] = [
    GetBlockField::Tx,
    GetBlockField::TxDetails,
    GetBlockField::TxPrevouts,
];

/// The state reads of each field, from the "State fields" column of the design's field table,
/// written out independently of `get_block::state_fields`.
const FIELD_READS: [(GetBlockField, BlockQueryFields); 25] = [
    (GetBlockField::Hash, BlockQueryFields::empty()),
    (GetBlockField::Height, BlockQueryFields::empty()),
    (GetBlockField::Confirmations, BlockQueryFields::DEPTH),
    (GetBlockField::Size, BlockQueryFields::BLOCK_INFO),
    (GetBlockField::Version, BlockQueryFields::HEADER),
    (GetBlockField::Time, BlockQueryFields::HEADER),
    (GetBlockField::Nonce, BlockQueryFields::HEADER),
    (GetBlockField::Solution, BlockQueryFields::HEADER),
    (GetBlockField::Bits, BlockQueryFields::HEADER),
    (GetBlockField::PreviousBlockHash, BlockQueryFields::HEADER),
    (GetBlockField::MerkleRoot, BlockQueryFields::HEADER),
    (GetBlockField::Difficulty, BlockQueryFields::HEADER),
    (
        GetBlockField::BlockCommitments,
        BlockQueryFields::HEADER.union(BlockQueryFields::SAPLING_TREE),
    ),
    (
        GetBlockField::AuthDataRoot,
        BlockQueryFields::AUTH_DATA_ROOT,
    ),
    (
        GetBlockField::FinalSaplingRoot,
        BlockQueryFields::SAPLING_TREE,
    ),
    (
        GetBlockField::FinalOrchardRoot,
        BlockQueryFields::ORCHARD_TREE,
    ),
    (GetBlockField::TxCount, BlockQueryFields::TRANSACTION_COUNT),
    (GetBlockField::Tx, BlockQueryFields::TRANSACTION_IDS),
    (
        GetBlockField::TxDetails,
        BlockQueryFields::TRANSACTIONS.union(BlockQueryFields::DEPTH),
    ),
    (
        GetBlockField::TxPrevouts,
        BlockQueryFields::TRANSACTIONS
            .union(BlockQueryFields::SPENT_OUTPUTS)
            .union(BlockQueryFields::DEPTH),
    ),
    (GetBlockField::ChainSupply, BlockQueryFields::BLOCK_INFO),
    (
        GetBlockField::ValuePools,
        BlockQueryFields::BLOCK_INFO.union(BlockQueryFields::PREVIOUS_BLOCK_INFO),
    ),
    (
        GetBlockField::Trees,
        BlockQueryFields::SAPLING_TREE
            .union(BlockQueryFields::ORCHARD_TREE)
            .union(BlockQueryFields::IRONWOOD_TREE),
    ),
    (
        GetBlockField::NextBlockHash,
        BlockQueryFields::NEXT_BLOCK_HASH,
    ),
    (GetBlockField::Hex, BlockQueryFields::RAW_BLOCK),
];

/// Returns the state reads of `field` from [`FIELD_READS`].
fn field_reads(field: GetBlockField) -> BlockQueryFields {
    FIELD_READS
        .iter()
        .find(|(row, _)| *row == field)
        .map(|(_, reads)| *reads)
        .expect("FIELD_READS has a row for every field")
}

/// Returns the fields whose bits are set in `mask`, plus the `tx` shape `shape` selects, if any.
fn fields_from(mask: u32, shape: usize) -> BTreeSet<GetBlockField> {
    NON_TX_FIELDS
        .into_iter()
        .enumerate()
        .filter(|(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, field)| field)
        .chain(TX_SHAPE_FIELDS.get(shape).copied())
        .collect()
}

/// Returns `object` with the optional keys whose bits are clear in `mask` set to `None`.
fn sparse_block_object(mut object: methods::BlockObject, mask: u32) -> methods::BlockObject {
    let keep = |bit: u32| mask & (1 << bit) != 0;
    macro_rules! clear {
        ($($bit:literal => $field:ident),* $(,)?) => {
            $(if !keep($bit) { object.$field = None; })*
        };
    }
    clear!(
        0 => confirmations,
        1 => size,
        2 => height,
        3 => version,
        4 => merkle_root,
        5 => block_commitments,
        6 => auth_data_root,
        7 => final_sapling_root,
        8 => final_orchard_root,
        9 => n_tx,
        10 => tx,
        11 => time,
        12 => nonce,
        13 => solution,
        14 => bits,
        15 => difficulty,
        16 => chain_supply,
        17 => value_pools,
        18 => trees,
        19 => previous_block_hash,
        20 => next_block_hash,
        21 => hex,
    );
    object
}

/// The JSON keys of the optional `BlockObject` fields, in the bit order of
/// [`sparse_block_object`].
const OPTIONAL_BLOCK_OBJECT_KEYS: [&str; 22] = [
    "confirmations",
    "size",
    "height",
    "version",
    "merkleroot",
    "blockcommitments",
    "authdataroot",
    "finalsaplingroot",
    "finalorchardroot",
    "nTx",
    "tx",
    "time",
    "nonce",
    "solution",
    "bits",
    "difficulty",
    "chainSupply",
    "valuePools",
    "trees",
    "previousblockhash",
    "nextblockhash",
    "hex",
];

/// Returns a `BlockObject` with every key set, for Mainnet block 1, with transaction IDs.
fn full_block_object() -> methods::BlockObject {
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("the test vector is a valid block");
    let plan = GetBlockPlan::new(fields_from(u32::MAX, 0));
    let queried = super::vectors::queried_block(&block, plan.state_fields, 0);
    let mut object = get_block::block_object(queried, &plan, &Network::Mainnet, true)
        .expect("the queried block has every part the plan reads");

    // Block 1 is before NU5 and has a next block only in a longer chain.
    object.final_orchard_root = Some([1; 32]);
    object.next_block_hash = Some(block.hash());
    object
}

proptest! {
    /// A plan reads the union of its fields' reads, without the reads other reads subsume: the
    /// transaction count when the IDs or transactions are read, and `BlockInfo` when only `size`
    /// needs it and the body is read. `authdataroot` reads the transactions.
    #[test]
    fn get_block_plan_reads_what_its_fields_need(mask in any::<u32>(), shape in 0..4usize) {
        let fields = fields_from(mask, shape);
        let plan = GetBlockPlan::new(fields.clone());
        let has = |field| fields.contains(&field);

        let union = fields
            .iter()
            .fold(BlockQueryFields::empty(), |reads, field| reads | field_reads(*field));
        prop_assert!(union.contains(plan.state_fields));
        prop_assert!((BlockQueryFields::TRANSACTION_COUNT | BlockQueryFields::BLOCK_INFO)
            .contains(union.difference(plan.state_fields)));

        let reads_transactions = has(GetBlockField::Tx)
            || has(GetBlockField::TxDetails)
            || has(GetBlockField::TxPrevouts)
            || has(GetBlockField::AuthDataRoot);
        prop_assert_eq!(
            plan.state_fields.contains(BlockQueryFields::TRANSACTION_COUNT),
            has(GetBlockField::TxCount) && !reads_transactions,
        );

        let reads_body = has(GetBlockField::Hex)
            || has(GetBlockField::TxDetails)
            || has(GetBlockField::TxPrevouts)
            || has(GetBlockField::AuthDataRoot);
        prop_assert_eq!(
            plan.state_fields.contains(BlockQueryFields::BLOCK_INFO),
            has(GetBlockField::ChainSupply)
                || has(GetBlockField::ValuePools)
                || (has(GetBlockField::Size) && !reads_body),
        );

        prop_assert_eq!(
            plan.tx_shape,
            [TxShape::Ids, TxShape::Objects, TxShape::ObjectsWithPrevouts].get(shape).copied(),
        );

        // A field query of the same fields has the same plan.
        if !fields.is_empty() {
            prop_assert_eq!(
                GetBlockPlan::try_from(&GetBlockQuery::Fields(fields.clone())).ok(),
                Some(plan.clone()),
            );
        }
        prop_assert_eq!(plan.fields, fields);
    }

    /// A field query naming more than one `tx` shape fails, naming them in field order.
    #[test]
    fn get_block_plan_rejects_conflicting_shapes(mask in any::<u32>(), shapes in 0..8usize) {
        let mut fields = fields_from(mask, usize::MAX);
        let named: Vec<_> = TX_SHAPE_FIELDS
            .into_iter()
            .enumerate()
            .filter(|(bit, _)| shapes & (1 << bit) != 0)
            .map(|(_, field)| field)
            .collect();
        fields.extend(named.iter().copied());

        let result = GetBlockPlan::try_from(&GetBlockQuery::Fields(fields.clone()));
        if fields.is_empty() {
            let error = result.expect_err("an empty field list is rejected");
            prop_assert_eq!((error.code(), error.message()), (-8, "empty field list"));
        } else if named.len() > 1 {
            let names: Vec<_> = named
                .iter()
                .map(|field| match field {
                    GetBlockField::Tx => "tx",
                    GetBlockField::TxDetails => "txdetails",
                    _ => "txprevouts",
                })
                .collect();
            let error = result.expect_err("conflicting shapes are rejected");
            prop_assert_eq!(error.code(), -8);
            prop_assert_eq!(error.message(), format!("conflicting fields: {}", names.join(", ")));
        } else {
            prop_assert!(result.is_ok());
        }
    }

    /// A `BlockObject` with any subset of its optional keys serializes to exactly those keys and
    /// `hash`, and deserializes back to the same object.
    #[test]
    fn block_object_round_trips_with_any_keys(mask in any::<u32>()) {
        let _init_guard = zebra_test::init();

        let object = sparse_block_object(full_block_object(), mask);
        let response = methods::GetBlockResponse::Object(Box::new(object));
        let json = serde_json::to_value(&response).expect("block objects serialize");

        let keys: BTreeSet<&str> = json
            .as_object()
            .expect("block objects serialize to JSON objects")
            .keys()
            .map(String::as_str)
            .collect();
        let expected_keys: BTreeSet<&str> = OPTIONAL_BLOCK_OBJECT_KEYS
            .into_iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, key)| key)
            .chain(["hash"])
            .collect();
        prop_assert_eq!(keys, expected_keys);

        let parsed: methods::GetBlockResponse =
            serde_json::from_value(json).expect("serialized block objects deserialize");
        prop_assert_eq!(parsed, response);
    }
}

/// [`FIELD_READS`] has one row for each field.
#[test]
fn field_reads_cover_every_field() {
    let rows: BTreeSet<GetBlockField> = FIELD_READS.iter().map(|(field, _)| *field).collect();
    let fields: BTreeSet<GetBlockField> =
        NON_TX_FIELDS.into_iter().chain(TX_SHAPE_FIELDS).collect();

    assert_eq!(rows.len(), FIELD_READS.len(), "each field has one row");
    assert_eq!(rows, fields);
}

/// The verbosity levels are the plans of their preset fields, which are the keys verbosity 1
/// returns.
#[test]
fn get_block_plan_presets() {
    // The keys of a verbosity 1 snapshot, plus `finalorchardroot`, which verbosity 1 returns
    // from NU5 and the snapshot's pre-NU5 block omits.
    let snapshot = include_str!("snapshots/get_block_verbose_hash_verbosity_1@mainnet_10.snap");
    let object = snapshot
        .splitn(3, "---\n")
        .nth(2)
        .expect("an insta snapshot has a header between two `---` lines");
    let object: serde_json::Value =
        serde_json::from_str(object).expect("the snapshot is a JSON object");
    let verbosity_1_fields: BTreeSet<GetBlockField> = object
        .as_object()
        .expect("verbosity 1 returns an object")
        .keys()
        .map(String::as_str)
        .chain(["finalorchardroot"])
        .map(|key| {
            serde_json::from_value(key.into())
                .unwrap_or_else(|error| panic!("{key} is a field name: {error}"))
        })
        .collect();
    assert_eq!(
        BTreeSet::from(get_block::VERBOSITY_1_FIELDS),
        verbosity_1_fields
    );

    let preset = |tx| -> BTreeSet<GetBlockField> {
        verbosity_1_fields
            .iter()
            .copied()
            .filter(|field| *field != GetBlockField::Tx)
            .chain([tx])
            .collect()
    };

    for (verbosity, tx) in [
        (1, GetBlockField::Tx),
        (2, GetBlockField::TxDetails),
        (3, GetBlockField::TxPrevouts),
    ] {
        assert_eq!(
            GetBlockPlan::try_from(&GetBlockQuery::Verbosity(verbosity)).unwrap(),
            GetBlockPlan::new(preset(tx)),
        );
    }
    assert_eq!(
        GetBlockPlan::try_from(&GetBlockQuery::Verbosity(0)).unwrap(),
        GetBlockPlan::new([GetBlockField::Hex].into()),
    );
    assert_eq!(get_block::VERBOSITY_1_FIELDS.len(), 21);
}
