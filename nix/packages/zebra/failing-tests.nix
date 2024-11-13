### Tests that fail on Nix, but aren’t skipped by `ZEBRA_SKIP_NETWORK_TESTS`.
{
  extraFeatures,
  lib,
  stdenv,
}:
## NB: On Darwin, `__darwinAllowLocalNetworking` would allow many of these tests to pass, but only
##     in a non-sandboxed environment.
[
  ## zebra-scan
  "scan_binary_starts"
  ## zebrad – acceptance
  "config_tests"
  "end_of_support_is_checked_at_start"
  "ephemeral_existing_directory"
  "ephemeral_missing_directory"
  "external_address"
  "misconfigured_ephemeral_existing_directory"
  "misconfigured_ephemeral_missing_directory"
  "persistent_mode"
  "trusted_chain_sync_handles_forks_correctly"
]
++ lib.optionals stdenv.hostPlatform.isDarwin [
  ## zebra-consensus
  "checkpoint::tests::block_higher_than_max_checkpoint_fail_test"
  "checkpoint::tests::checkpoint_drop_cancel_test"
  "checkpoint::tests::continuous_blockchain_no_restart"
  "checkpoint::tests::continuous_blockchain_restart"
  "checkpoint::tests::hard_coded_mainnet_test"
  "checkpoint::tests::multi_item_checkpoint_list_test"
  "checkpoint::tests::single_item_checkpoint_list_test"
  "checkpoint::tests::wrong_checkpoint_hash_fail_test"
  "router::tests::round_trip_checkpoint_test"
  "router::tests::verify_checkpoint_test"
  "router::tests::verify_fail_add_block_checkpoint_test"
  "router::tests::verify_fail_no_coinbase_test"
  ## zebra-grpc
  "tests::snapshot::test_grpc_response_data"
  "tests::vectors::test_grpc_methods_mocked"
  ## zebra-rpc
  "server::tests::vectors::rpc_server_spawn_parallel_threads"
  "server::tests::vectors::rpc_server_spawn_single_thread"
  "server::tests::vectors::rpc_server_spawn_unallocated_port_single_thread"
  "server::tests::vectors::rpc_server_spawn_unallocated_port_single_thread_shutdown"
  "server::tests::vectors::rpc_sever_spawn_unallocated_port_parallel_threads"
  "server::tests::vectors::rpc_sever_spawn_unallocated_port_parallel_threads_shutdown"
  ## zebrad
  "components::inbound::tests::real_peer_set::inbound_block_empty_state_notfound"
  "components::inbound::tests::real_peer_set::inbound_peers_empty_address_book"
  "components::inbound::tests::real_peer_set::inbound_tx_empty_state_notfound"
  "components::inbound::tests::real_peer_set::outbound_tx_partial_response_notfound"
  "components::inbound::tests::real_peer_set::outbound_tx_unrelated_response_notfound"
  ## zebrad – acceptance
  "downgrade_state_format"
  "new_state_format"
  "regtest_block_templates_are_valid_block_submissions"
  "start_args"
  "start_no_args"
  "update_state_format"
  "zebra_state_conflict"
  "zebra_zcash_listener_conflict"
]
++ lib.optionals stdenv.hostPlatform.isLinux [
  ## zebrad – acceptance
  "non_blocking_logger"
]
## These tests seem to only fail when the `elasticsearch` feature is enabled on MacOS.
++ lib.optionals (stdenv.hostPlatform.isDarwin && lib.elem "elasticsearch" extraFeatures) [
  ## zebra-rpc
  "methods::tests::snapshot::test_rpc_response_data"
  "methods::tests::snapshot::test_z_get_treestate"
  "methods::tests::vectors::rpc_getaddresstxids_invalid_arguments"
  "methods::tests::vectors::rpc_getaddresstxids_response"
  "methods::tests::vectors::rpc_getaddressutxos_response"
  "methods::tests::vectors::rpc_getbestblockhash"
  "methods::tests::vectors::rpc_getblock"
  "methods::tests::vectors::rpc_getblockcount"
  "methods::tests::vectors::rpc_getblockcount_empty_state"
  "methods::tests::vectors::rpc_getblockhash"
  "methods::tests::vectors::rpc_getmininginfo"
  "methods::tests::vectors::rpc_getnetworksolps"
  "methods::tests::vectors::rpc_getpeerinfo"
  "methods::tests::vectors::rpc_getrawtransaction"
  "methods::tests::vectors::rpc_submitblock_errors"
  ## zebra-scan
  "service::tests::scan_service_registers_keys_correctly"
  "tests::vectors::scanning_zecpages_from_populated_zebra_state"
  ## zebra-state
  "service::read::tests::vectors::empty_read_state_still_responds_to_requests"
  "service::read::tests::vectors::populated_read_state_responds_correctly"
  "service::tests::chain_tip_sender_is_updated"
  "service::tests::empty_state_still_responds_to_requests"
  "service::tests::state_behaves_when_blocks_are_committed_in_order"
  "service::tests::state_behaves_when_blocks_are_committed_out_of_order"
  "service::tests::value_pool_is_updated"
  ## zebra-state – basic
  "check_transcripts_mainnet"
  "check_transcripts_testnet"
  ## zebrad
  "components::inbound::tests::fake_peer_set::caches_getaddr_response"
  "components::inbound::tests::fake_peer_set::inbound_block_height_lookahead_limit"
  "components::inbound::tests::fake_peer_set::mempool_advertise_transaction_ids"
  "components::inbound::tests::fake_peer_set::mempool_push_transaction"
  "components::inbound::tests::fake_peer_set::mempool_requests_for_transactions"
  "components::inbound::tests::fake_peer_set::mempool_transaction_expiration"
  "components::mempool::tests::vector::mempool_cancel_downloads_after_network_upgrade"
  "components::mempool::tests::vector::mempool_cancel_mined"
  "components::mempool::tests::vector::mempool_failed_download_is_not_rejected"
  "components::mempool::tests::vector::mempool_failed_verification_is_rejected"
  "components::mempool::tests::vector::mempool_queue"
  "components::mempool::tests::vector::mempool_reverifies_after_tip_change"
  "components::mempool::tests::vector::mempool_service_basic"
  "components::mempool::tests::vector::mempool_service_disabled"
  ## zebrad – acceptance
  "db_init_outside_future_executor"
  "nu6_funding_streams_and_coinbase_balance"
  "validate_regtest_genesis_block"
]
