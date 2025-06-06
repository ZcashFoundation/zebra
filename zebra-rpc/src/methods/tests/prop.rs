//! Randomised property tests for RPC methods.

use std::{
    collections::{HashMap, HashSet},
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
use zebra_consensus::ParameterCheckpoint;
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::mempool;
use zebra_state::{BoxError, GetBlockTemplateChainInfo};
use zebra_test::mock_service::MockService;

use crate::methods::{
    self,
    types::{
        get_blockchain_info,
        get_raw_mempool::{GetRawMempool, MempoolObject},
    },
};

use super::super::{
    AddressBalance, AddressStrings, NetworkUpgradeStatus, RpcImpl, RpcServer, SentTransactionHash,
};

proptest! {
    /// Test that when sending a raw transaction, it is received by the mempool service.
    #[test]
    fn mempool_receives_raw_tx(transaction in any::<Transaction>(), network in any::<Network>()) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let hash = SentTransactionHash(transaction.hash());

            let transaction_bytes = transaction.zcash_serialize_to_vec()?;

            let transaction_hex = hex::encode(&transaction_bytes);

            let send_task = tokio::spawn(async move { rpc.send_raw_transaction(transaction_hex, None).await });

            let unmined_transaction = UnminedTx::from(transaction);
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

            let unmined_transaction = UnminedTx::from(transaction);
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
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (mut mempool, mut state, rpc, mempool_tx_queue) = mock_services(network, NoChainTip);

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let tx = hex::encode(&transaction.zcash_serialize_to_vec()?);
            let req = mempool::Request::Queue(vec![UnminedTx::from(transaction).into()]);
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
            // Note: this depends on `SHOULD_USE_ZCASHD_ORDER` being true.
            let (expected_response, mempool_query) = {
                let mut expected_response = transactions.clone();

                expected_response.sort_by_cached_key(|tx| {
                    // zcashd uses modified fee here but Zebra doesn't currently
                    // support prioritizing transactions
                    std::cmp::Reverse((
                        i64::from(tx.miner_fee) as u128 * zebra_chain::block::MAX_BLOCK_BYTES as u128
                            / tx.transaction.size as u128,
                        // transaction hashes are compared in their serialized byte-order.
                        tx.transaction.id.mined_id(),
                    ))
                });

                let expected_response = expected_response
                    .iter()
                    .map(|tx| tx.transaction.id.mined_id().encode_hex::<String>())
                    .collect::<Vec<_>>();

                let transaction_dependencies = Default::default();
                let expected_response = if verbose.unwrap_or(false) {
                    let map = transactions
                        .iter()
                        .map(|unmined_tx| {
                            (
                                unmined_tx.transaction.id.mined_id().encode_hex(),
                                MempoolObject::from_verified_unmined_tx(
                                    unmined_tx,
                                    &transactions,
                                    &transaction_dependencies,
                                ),
                            )
                        })
                        .collect::<HashMap<_, _>>();
                    GetRawMempool::Verbose(map)
                } else {
                    GetRawMempool::TxIds(expected_response)
                };

                let mempool_query = mempool
                    .expect_request(mempool::Request::FullTransactions)
                    .map_ok(|r| r.respond(mempool::Response::FullTransactions {
                        transactions,
                        transaction_dependencies,
                        last_seen_tip_hash: [0; 32].into(),
                    }));

                (expected_response, mempool_query)
            };

            let (rpc_rsp, _) = tokio::join!(rpc.get_raw_mempool(verbose), mempool_query);

            prop_assert_eq!(rpc_rsp?, expected_response);

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
            let rpc_rsp = rpc.get_raw_transaction(invalid_txid, Some(1)).await;

            check_err_code(rpc_rsp, ErrorCode::ServerError(-5))?;

            // Check that no further requests were made.
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // Now check the unknown TXID.
            let mempool_query = mempool
                .expect_request(mempool::Request::TransactionsByMinedId([unknown_txid].into()))
                .map_ok(|r| r.respond(mempool::Response::Transactions(vec![])));

            let state_query = state
                .expect_request(zebra_state::ReadRequest::Transaction(unknown_txid))
                .map_ok(|r| r.respond(zebra_state::ReadResponse::Transaction(None)));

            let rpc_query = rpc.get_raw_transaction(unknown_txid.encode_hex(), Some(1));

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
                            max_time: DateTime32::now()
                        }));
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
                            max_time: DateTime32::now()
                        }));
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

                }
            };

            let (response, _) = tokio::join!(response_fut, mock_state_handler);

            let response = response.expect("should succeed with genesis block info");

            prop_assert_eq!(response.best_block_hash, genesis_block.header.hash());
            prop_assert_eq!(response.chain, network.bip70_network_name());
            prop_assert_eq!(response.blocks, Height::MIN);
            prop_assert_eq!(response.value_pools, get_blockchain_info::Balance::value_pools(ValueBalance::zero(), None));

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
        let address_strings = AddressStrings {
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
                    responder.respond(zebra_state::ReadResponse::AddressBalance { balance, received: Default::default() })
                });

            // Await the RPC call and the state query
            let (response, state_query_result) = join!(call, state_query);

            state_query_result?;

            // Check that response contains the expected balance
            let received_balance = response?;

            prop_assert_eq!(received_balance, AddressBalance { balance: balance.into() });

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

            let address_strings = AddressStrings {
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
            let tx_unmined = UnminedTx::from(tx);
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
            let expected_request = zebra_state::ReadRequest::Transaction(transaction_hash);
            let response = zebra_state::ReadResponse::Transaction(None);

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

                let tx_unmined = UnminedTx::from(tx.clone());
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
                let response = zebra_state::ReadResponse::Transaction(None);

                // we use `expect_request_that` because we can't guarantee the state request order
                state
                    .expect_request_that(|request| {
                        matches!(request, zebra_state::ReadRequest::Transaction(_))
                    })
                    .await?
                    .respond(response);
            }

            // each transaction will be retried
            for tx in txs.clone() {
                let expected_request =
                    mempool::Request::Queue(vec![mempool::Gossip::Tx(UnminedTx::from(tx))]);
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
        block_verifier_router,
        MockSyncStatus::default(),
        chain_tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    (mempool, state, rpc, mempool_tx_queue)
}
