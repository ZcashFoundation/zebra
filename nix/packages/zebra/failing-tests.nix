### Tests that fail on Nix, but aren’t skipped by `ZEBRA_SKIP_NETWORK_TESTS`.
{
  lib,
  stdenv,
}:
[
  ## zebra-scan
  "scan_binary_starts"
  ## zebrad – acceptance
  "config_tests"
  "trusted_chain_sync_handles_forks_correctly"
]
++ lib.optionals stdenv.hostPlatform.isDarwin [
  ## zebrad – acceptance
  "regtest_block_templates_are_valid_block_submissions"
]
++ lib.optionals stdenv.hostPlatform.isLinux [
  ## zebrad – acceptance
  "config_tests"
  "end_of_support_is_checked_at_start"
  "ephemeral_existing_directory"
  "ephemeral_missing_directory"
  "external_address"
  "misconfigured_ephemeral_existing_directory"
  "misconfigured_ephemeral_missing_directory"
  "non_blocking_logger"
  "persistent_mode"
]
