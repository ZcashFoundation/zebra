# zcashd-compat Integration Tests

Integration tests for zebrad's **zcashd-compat mode**, in which zebrad acts as
the consensus/P2P/mempool backend and zcashd handles the wallet.  These tests
validate the full two-process deployment end-to-end.

## Two Modes

### Managed — Regtest (CI)

The test harness spawns a fresh zebrad and zcashd on randomised ports, runs
the full test suite including block mining and wallet sends, then tears
everything down.  No external infrastructure is required.

```
TEST_ZCASHD_COMPAT=1                          (required)
TEST_ZCASHD_PATH=/path/to/zcashd        (optional — uses embedded download if unset)
```

Run:

```console
# Via make
make compat-test-regtest TEST_ZCASHD_PATH=/path/to/zcashd

# Via cargo directly
TEST_ZCASHD_COMPAT=1 \
  TEST_ZCASHD_PATH=/path/to/zcashd \
  cargo nextest run --profile zcashd-compat-integration --run-ignored=only
```

The suite is **opt-in**: every test is `#[ignore]`d and skipped unless
`TEST_ZCASHD_COMPAT=1` is set. In CI it runs on a weekly schedule and on
manual dispatch — see
[`.github/workflows/zcashd-compat-regtest.yml`](../../../../.github/workflows/zcashd-compat-regtest.yml).
It is not a required PR check yet.

### Reorg Stress Tests

The regtest suite includes reorg regression coverage for zcashd's Zebra sync
worker:

- `zcashd_compat_reorg_basic_depth1` verifies normal depth-1 reorg convergence.
- `zcashd_compat_reorg_equal_work_race` pins the equal-work, same-height degraded
  state and verifies recovery after Zebra extends its branch.
- `zcashd_compat_reorg_deep_depth33` verifies a 33-block replacement branch.
- `zcashd_compat_reorg_deep_depth80` verifies an 80-block replacement branch.
- `zcashd_compat_reorg_deep_restart_recovers` verifies that a deep replacement
  branch remains healthy after a supervised zcashd restart.
- `zcashd_compat_reorg_restart_after_reorg` is an opt-in slow probe for zcashd
  supervisor restart and block-index reload after several Zebra-side reorgs.
  Skipped unless `TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG=1`.
- `zcashd_compat_reorg_restart_cycles` interleaves reorg and restart across three
  cycles to verify trusted-boundary advancement on disk.
- `zcashd_compat_reorg_restart_deep_chain` verifies VerifyDB window coverage on a
  long trusted chain after reorg and restart. Opt-in via
  `TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG=1`.
- `zcashd_compat_reorg_context_zebra_tip_behind_recovers` verifies no sticky failure
  when Zebra shrinks after a paused reorg has converged (end-to-end recovery to
  `zebra_tip_matched`).
- `zcashd_compat_reorg_zebra_tip_behind_local` verifies the recoverable
  `zebra_tip_behind_local` degraded state and requires recovery after Zebra
  mines a replacement branch.
- `zcashd_compat_reorg_churn` repeats small reorgs and occasional mid-sync
  depth-1 churn.

Depth >1 forced reorgs pause the managed zcashd process with `SIGSTOP` while
Zebra invalidates and mines the replacement branch, then resume it with
`SIGCONT`. This keeps zcashd from observing an intermediate shorter Zebra best
chain during the test orchestration.

The churn test defaults to 30 cycles. Override it for soak runs with
`TEST_ZCASHD_COMPAT_REORG_ITERATIONS`:

```console
make compat-test-soak \
  TEST_ZCASHD_PATH=/path/to/zcashd \
  TEST_ZCASHD_COMPAT_REORG_ITERATIONS=500
```

### External — Mainnet / Testnet (deployment validation)

The test harness connects to pre-running zebrad and zcashd instances.
All tests that require block mining or wallet spends skip automatically.
No writes are performed on a live network.

```
TEST_ZCASHD_COMPAT=1                          (required)
TEST_ZCASHD_COMPAT_NETWORK=Mainnet      (or Testnet)
TEST_ZEBRAD_RPC_ADDR=127.0.0.1:8232    (zebrad main RPC)
TEST_ZCASHD_RPC_ADDR=127.0.0.1:28232    (zcashd own RPC)

# Authentication — provide one of:
TEST_ZCASHD_COOKIE_FILE=/path/to/.cookie    (preferred)
TEST_ZCASHD_RPC_USER=username               (alternative)
TEST_ZCASHD_RPC_PASSWORD=password
```

Run:

```console
# Via make (addresses can be overridden as Make vars)
make compat-test-mainnet \
  TEST_ZEBRAD_RPC_ADDR=127.0.0.1:8232 \
  TEST_ZCASHD_RPC_ADDR=127.0.0.1:28232 \
  TEST_ZCASHD_COOKIE_FILE=/home/user/.zcash/.cookie

make compat-test-testnet \
  TEST_ZEBRAD_RPC_ADDR=127.0.0.1:18232 \
  TEST_ZCASHD_RPC_ADDR=127.0.0.1:18233 \
  TEST_ZCASHD_COOKIE_FILE=/home/user/.zcash/testnet3/.cookie

# Via cargo directly (mainnet example)
TEST_ZCASHD_COMPAT=1 \
  TEST_ZCASHD_COMPAT_NETWORK=Mainnet \
  TEST_ZEBRAD_RPC_ADDR=127.0.0.1:8232 \
  TEST_ZCASHD_RPC_ADDR=127.0.0.1:28232 \
  TEST_ZCASHD_COOKIE_FILE=/home/user/.zcash/.cookie \
  cargo nextest run --profile zcashd-compat-external --run-ignored=only
```

### Skip behaviour

If `TEST_ZCASHD_COMPAT` is not set, every test prints a message and exits
`Ok(())` immediately.  The skip is silent in CI output — no failures, no noise.

If `TEST_ZCASHD_COMPAT_NETWORK` is set to `Mainnet` or `Testnet` but
the required address or auth variables are missing, the test suite returns an
error (misconfiguration, not a skip).

## Environment Variables

| Variable | Required | Purpose |
|---|---|---|
| `TEST_ZCASHD_COMPAT` | Always | Enable the suite (set to any non-empty value) |
| `TEST_ZCASHD_PATH` | No | Path to a zcashd binary; uses embedded download if absent |
| `TEST_ZCASHD_COMPAT_NETWORK` | External only | `Mainnet` or `Testnet`; absent = Regtest/managed |
| `TEST_ZEBRAD_RPC_ADDR` | External only | zebrad main RPC (`host:port`) |
| `TEST_ZCASHD_RPC_ADDR` | External only | zcashd own RPC (`host:port`) |
| `TEST_ZCASHD_COOKIE_FILE` | External (preferred) | Path to zcashd cookie file |
| `TEST_ZCASHD_RPC_USER` | External (fallback) | zcashd RPC username |
| `TEST_ZCASHD_RPC_PASSWORD` | External (fallback) | zcashd RPC password |
| `TEST_ZCASHD_COMPAT_REORG_ITERATIONS` | No | Reorg churn cycles; defaults to 30 in tests and 500 in `make compat-test-soak` |
| `TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG` | No | Set to `1` to run slow restart-after-reorg probes |

## Test Inventory

| Test function | Module | Regtest | Mainnet/Testnet |
|---|---|---|---|
| `zcashd_compat_both_processes_start` | startup | Full check — asserts `chain == "regtest"` | Full check — asserts `chain == "main"/"test"` |
| `zcashd_compat_sidecar_follows_tip` | startup | Mines blocks, asserts zcashd follows Zebra's tip and peers with Zebra alone | Checks the tips agree (no mining) |
| `zcashd_compat_miner_rpcs_disabled` | startup | Asserts `getblocktemplate` returns method-not-found on the sidecar | Same read-only check |
| `zcashd_compat_height_and_hash_agree` | chain | Mines 5, asserts count == 5 on both sides | Cross-checks current tip (no mining) |
| `zcashd_compat_getblock_hash_consistent` | chain | Mines 3, checks heights 1–3 | Checks last 3 blocks at current tip |
| `zcashd_compat_wallet_address_generation` | wallet | `z_getnewaccount` + `z_getaddressforaccount` return a non-empty unified address | Same check |
| `zcashd_compat_wallet_initial_balance_zero` | wallet | Asserts `getbalance` is zero | **Skipped** (live wallet may have funds) |
| `zcashd_compat_getwalletinfo_fields_present` | wallet | Full check | Full check |
| `zcashd_compat_shielded_tx_in_mempool` | tx_flow | Mines coinbase to the account's Sapling receiver, `z_sendmany`s, polls zebrad mempool | Validates `getmempoolinfo` structure only |
| `zcashd_compat_shielded_tx_confirms` | tx_flow | Shielded send + mines + checks confirmations on both sides | **Skipped** |
| `zcashd_compat_zebrad_abrupt_kill` | resilience | Mines 3, SIGKILLs zebrad, asserts it was killed; the harness kills the orphaned sidecar | **Skipped** (don't own process) |
| `zcashd_compat_zebrad_graceful_shutdown_stops_zcashd` | resilience | SIGTERMs zebrad, asserts the supervised zcashd also exits | **Skipped** (unix only; don't own process) |
| `zcashd_compat_zcashd_restarts_after_exit` | resilience | SIGTERMs zcashd, waits for supervisor restart | **Skipped** (unix only; don't own process) |
| `zcashd_compat_peer_connectivity` | network | **Skipped** (regtest has no peers) | Asserts at least one peer connected |
| `zcashd_compat_mempool_info_valid` | network | Structural check only | Structural check (mempool typically non-empty) |
| `zcashd_compat_historical_block_consistent` | network | **Skipped** (no canonical block 1 on fresh chain) | Block hash at height 1 agrees on both sides |
| `zcashd_compat_reorg_basic_depth1` | reorg | Depth-1 reorg convergence | **Skipped** |
| `zcashd_compat_reorg_equal_work_race` | reorg | Equal-work degraded state and recovery | **Skipped** |
| `zcashd_compat_reorg_deep_depth33` | reorg | 33-block replacement branch convergence | **Skipped** |
| `zcashd_compat_reorg_deep_depth80` | reorg | 80-block replacement branch convergence | **Skipped** |
| `zcashd_compat_reorg_deep_restart_recovers` | reorg | Deep replacement branch remains healthy after restart | **Skipped** |
| `zcashd_compat_reorg_restart_after_reorg` | reorg | **Opt-in:** slow supervised zcashd restart after several reorgs | **Skipped** |
| `zcashd_compat_reorg_restart_cycles` | reorg | **Opt-in:** interleaved reorg-then-restart across three cycles | **Skipped** |
| `zcashd_compat_reorg_restart_deep_chain` | reorg | **Opt-in:** VerifyDB window on long trusted chain after reorg + restart | **Skipped** |
| `zcashd_compat_reorg_zebra_tip_behind_local` | reorg | Recoverable Zebra-tip-behind-local state and required recovery | **Skipped** |
| `zcashd_compat_reorg_context_zebra_tip_behind_recovers` | reorg | No sticky failure on tip-behind after paused reorg convergence | **Skipped** |
| `zcashd_compat_reorg_churn` | reorg | Repeated small reorg stress loop | **Skipped** |

## Prerequisites for External Mode

Before running against mainnet or testnet:

1. zebrad must be running with zcashd-compat enabled and fully synced to the
   network tip.
2. zcashd must be running in zebra-compat mode, connected to that zebrad via
   the compat RPC channel.
3. Both processes must be reachable from the test runner via the addresses in
   `TEST_ZEBRAD_RPC_ADDR` and `TEST_ZCASHD_RPC_ADDR`.
4. zcashd's cookie file path or explicit credentials must be provided.

A typical production layout uses the cookie file (`~/.zcash/.cookie` on
mainnet, `~/.zcash/testnet3/.cookie` on testnet) — this is the most
straightforward way to authenticate.

## Module Structure

```
zebrad/tests/common/
├── zcashd_compat.rs          module root — skip guard, ZcashdRpcClient,
│                              env var constants, setup_zcashd_compat()
└── zcashd_compat/
    ├── config.rs              build_zcashd_compat_config() (regtest only),
    │                          expected_zebrad_chain_name(),
    │                          expected_zcashd_chain_name(),
    │                          read_test_network_kind()
    ├── launch.rs              ZcashdCompatSetup, spawn_zebrad_with_zcashd_compat(),
    │                          connect_to_external_zcashd_compat(), wait_for_zcashd_rpc()
    ├── startup.rs             both_processes_start, sidecar_follows_tip,
    │                          miner_rpcs_disabled
    ├── chain.rs               height_and_hash_agree, getblock_hash_consistent
    ├── wallet.rs              address_generation, initial_balance_zero,
    │                          getwalletinfo_fields_present
    ├── tx_flow.rs             shielded_tx_in_mempool, shielded_tx_confirms
    ├── resilience.rs          zebrad_abrupt_kill,
    │                          zebrad_graceful_shutdown_stops_zcashd,
    │                          zcashd_restarts_after_exit
    ├── network.rs             peer_connectivity, mempool_info_valid,
    │                          historical_block_consistent
    └── reorg.rs               basic_depth1, equal_work_race,
                               deep_reorg_depth33, deep_reorg_depth80,
                               deep_reorg_restart_recovers,
                               restart_after_reorg, restart_cycles,
                               restart_deep_chain, zebra_tip_behind_local,
                               reorg_context_zebra_tip_behind_recovers, churn
```

Entry points are the `#[tokio::test] #[ignore]` functions in
`zebrad/tests/integration/zcashd_compat.rs` (all prefixed `zcashd_compat_`).

## Adding a New Test

1. Choose the right submodule (or create a new one).
2. Write an `async fn my_test() -> Result<()>` function:

   ```rust
   pub async fn my_test() -> Result<()> {
       let Some(setup) = setup_zcashd_compat().await? else {
           return Ok(());   // TEST_ZCASHD_COMPAT unset — silent skip
       };

       if !setup.can_mutate() {
           // On mainnet/testnet: read-only check or skip
           return setup.teardown();
       }

       // Regtest path — free to mine, send, inspect state
       use crate::common::regtest::MiningRpcMethods;
       setup.zebra_client.generate(1).await?;
       // ...

       setup.teardown()
   }
   ```

3. Add a corresponding entry point in `zebrad/tests/acceptance.rs`:

   ```rust
   #[tokio::test]
   #[ignore]
   async fn zcashd_compat_my_test() -> Result<()> {
       common::zcashd_compat::my_module::my_test().await
   }
   ```

Key rules:

- Call `setup.teardown()` on every exit path that owns a managed process.
- Guard all writes (`generate`, `sendtoaddress`, `z_sendmany`) behind `setup.can_mutate()`.
- Use `setup.zebra_client` (unauthenticated) for zebrad and `setup.zcashd_client` (Basic Auth) for zcashd.
