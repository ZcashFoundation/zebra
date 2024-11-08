### Tests that fail on Nix, but aren’t skipped by `ZEBRA_SKIP_NETWORK_TESTS`.
{
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
