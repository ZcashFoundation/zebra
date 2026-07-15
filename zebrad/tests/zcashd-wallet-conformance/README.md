# zcashd wallet/RPC conformance against the Zebra + sidecar pairing

This harness runs zcashd wallet RPC tests against a **Zebra + zcashd-sidecar**
pairing (zcashd-compat mode), to prove the sidecar's wallet and RPC surface
behaves the same when Zebra — not a mesh of standalone zcashds — is its network
and miner.

## Why a harness is needed

The upstream zcashd RPC tests (`qa/rpc-tests/`) assume each node is a
self-sufficient zcashd that **mines its own blocks** (`node.generate()`) and
**connects to other nodes in a P2P mesh** (`connect_nodes_bi`). A zcashd-compat
sidecar can do neither: its miner RPCs are removed, and it hard-locks to a
single Zebra peer. So the _network layer_ has to come from Zebra, while the
sidecar keeps its wallet.

## Architecture

```text
                 ┌─────────── one Zebra (regtest, miner + hub) ───────────┐
                 │  mines via generatetoaddress, relays blocks & txs      │
                 └──▲───────────────▲───────────────▲────────────────────-┘
                    │ P2P           │ P2P           │ P2P
              ┌─────┴────┐    ┌─────┴────┐    ┌─────┴────┐
              │ zcashd 0 │    │ zcashd 1 │    │ zcashd 2 │   (sidecars: wallets)
              └──────────┘    └──────────┘    └──────────┘
```

- **N sidecars fan into one Zebra.** No P2P mesh between sidecars — Zebra relays,
  so a tx or block on one sidecar reaches the others.
- **`node[i].generate(n)`** mines `n` regtest blocks on Zebra with the coinbase
  paid to node `i`'s own wallet, via Zebra's regtest **`generatetoaddress`** RPC,
  then waits for every sidecar to follow. This reproduces "each node mines its
  own coinbase" without any zcashd mining.
- Every other RPC passes straight through to the node's sidecar.

`generatetoaddress` was added to Zebra for this (regtest-only; it mines to a
caller-specified address instead of the configured `mining.miner_address`).

## Shielded-first wallet

The sidecar zcashd build is shielded-first: the legacy transparent
`getnewaddress` is disabled, and transparent coinbase paid to a unified-address
receiver is not credited. So the harness drives the wallet through the modern
account / unified-address / shielded `z_*` RPCs: `z_getnewaccount`,
`z_getaddressforaccount`, `z_getbalanceforaccount`, and `z_sendmany`. Coinbase
is mined to each account's Sapling receiver.

`z_getbalanceforaccount` is purely confirmation-gated — it does not apply
coinbase maturity itself — so spendable (mature) coinbase is queried at
`COINBASE_MATURITY + 1` confirmations.

## What makes the pairing work on Regtest

A few Regtest-specific behaviours are required for a zcashd sidecar to follow a
Zebra Regtest chain; the corresponding Zebra fixes live on this branch:

- **Fixed difficulty.** Zebra pins every Regtest block to the powLimit (no
  retargeting), matching zcashd's `fPowNoRetargeting`, so the sidecar accepts
  the headers instead of rejecting them as `bad-diffbits`.
- **Minimal block-time advance.** Zebra advances Regtest block time minimally
  (just above the median-time-past) instead of clamping `now()` to
  `median-time-past + 90 min`. A fresh chain starts from the 2011-era genesis,
  so clamping to real time would race chain time ~90 min per block and outrun
  the sidecar's block-time window.
- **`getheaders` always answered.** Zebra replies to `getheaders` with a
  `headers` message even when it has none, so the sidecar's request never hangs.
- **Frozen-clock tx relay (`setmocktime`).** The sidecar's clock is frozen with
  `mocktime` so a genesis-era tip reads as recent (not IBD, wallet enabled). But
  with the clock frozen, zcashd's transaction-relay trickle timer never elapses,
  so it never flushes queued tx invs to Zebra (block invs are sent
  unconditionally, so block sync is unaffected). The harness therefore advances
  the sidecar clock while waiting for a wallet tx to reach Zebra's mempool, as
  zcashd's own test framework advances mocktime. Real-clock nodes (mainnet /
  testnet) don't hit this.

## Files

| File | Purpose |
| --- | --- |
| `harness.py` | The reusable harness: launches Zebra + N sidecars, mines per-node via Zebra, advances the sidecar clock to drive tx relay, cleans up. |
| `wallet_conformance.py` | Single-node wallet conformance: account creation, shielded coinbase, coinbase maturity, account balance, a `z_sendmany` shielded spend that propagates over P2P to Zebra, is mined, confirms, and credits the recipient. |
| `wallet_multinode.py` | 3-node shielded conformance: three sidecars fan into one Zebra, each mining its own shielded coinbase, with a shielded node-to-node transfer. |
| `run_upstream.py` | Adapter that runs an **unmodified** upstream test by monkeypatching the framework's node-lifecycle and topology primitives (see limits below). |

## Requirements

```sh
export ZEBRAD_BIN=/path/to/zebrad          # built from this branch (has generatetoaddress + zcashd-compat)
export ZCASHD_BIN=/path/to/sidecar/zcashd  # a P2P-sidecar zcashd build (ZcashFoundation/zcashd zebra-compat release)
```

## Running

Ported conformance tests (self-contained, no upstream tree needed):

```sh
python3 wallet_conformance.py
python3 wallet_multinode.py
```

Both exit 0 on success and print `PASS: ...`.

An unmodified upstream test, via the adapter:

```sh
export ZCASHD_RPC_TESTS_DIR=/path/to/zcash/qa/rpc-tests
python3 run_upstream.py <test>.py
```

Set `HARNESS_TRACING_FILTER` (e.g. `info,zebra_network=debug`) to pass a
`[tracing] filter` through to zebrad for debugging. It is deliberately **not**
`ZEBRA_`-prefixed, since zebrad treats `ZEBRA_*` env vars as config-field
overrides and fatal-errors on an unknown field.

## `run_upstream.py` scope and limits

The adapter reuses zcashd's own `test_framework`, swapping only the primitives
that assume a standalone, mining, mesh-capable zcashd (`start_nodes`,
`connect_nodes*`, `initialize_chain*`, `stop_nodes`). It can run an upstream
test **only if that test fits the harness's fixed Regtest configuration**:

- **Shared, fixed network-upgrade schedule.** Zebra mines the blocks the sidecar
  validates, so both must agree on activation heights. The harness activates
  every upgrade through NU5 at height 1. Tests that pass their own
  `nuparams=...:<height>` via `extra_args` (most wallet tests do) are asking for
  a _different_ schedule and can't be honoured without reconfiguring Zebra's
  Regtest params to match per test.
- **No deprecated / transparent-only RPCs.** Tests relying on
  `-allowdeprecated=getnewaddress` (or the transparent wallet in general) fail on
  the shielded-first sidecar.
- **No miner RPCs on the sidecar.** `getblocktemplate` / `submitblock` /
  `generate` are removed by design — mining is Zebra's job.
- **No forks / reorgs / network splits.** All sidecars follow one Zebra chain, so
  tests that split the mesh to create competing tips have no equivalent.
- **No cached-chain assumptions.** `initialize_chain` is a no-op; the harness
  starts every node on an empty Regtest chain, so tests asserting a pre-seeded
  height/UTXO set need to mine that state themselves.

The self-contained `wallet_conformance.py` / `wallet_multinode.py` exist because
they exercise the full modern wallet surface within these constraints; they are
the primary demonstration of the pairing.

## Validation status

Validated end-to-end in the Zebra tree against a live sidecar:

- `wallet_conformance.py` — **PASS.** Zebra mines 105 Regtest blocks paid to the
  sidecar's shielded account; the sidecar follows over P2P; coinbase matures;
  a `z_sendmany` shielded spend propagates back to Zebra, is mined, confirms,
  and credits the recipient account.
- `wallet_multinode.py` — **PASS.** Three sidecars fan into one Zebra, each mines
  its own shielded coinbase, and a shielded transfer moves value node-to-node
  through Zebra.

The sidecar↔Zebra P2P + wallet path is also covered by the Rust integration
suite (`zebrad/tests/common/zcashd_compat/`), which mines on Zebra and reads
wallet state back from the sidecar.
