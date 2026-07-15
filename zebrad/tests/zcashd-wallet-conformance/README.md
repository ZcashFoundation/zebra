# zcashd wallet/RPC conformance against the Zebra + sidecar pairing

This harness runs zcashd's own RPC test suite against a **Zebra + zcashd-sidecar**
pairing (zcashd-compat mode), to prove the sidecar's wallet and RPC surface
behaves the same when Zebra — not a mesh of standalone zcashds — is its network
and miner.

## Why a harness is needed

The upstream zcashd RPC tests (`qa/rpc-tests/`) assume each node is a
self-sufficient zcashd that **mines its own blocks** (`node.generate()`) and
**connects to other nodes in a P2P mesh** (`connect_nodes_bi`). A zcashd-compat
sidecar can do neither: its miner RPCs are removed, and it hard-locks to a
single Zebra peer. So the *network layer* has to come from Zebra, while the
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

* **N sidecars fan into one Zebra.** No P2P mesh between sidecars — Zebra relays,
  so a tx or block on one sidecar reaches the others.
* **`node[i].generate(n)`** mines `n` regtest blocks on Zebra with the coinbase
  paid to node `i`'s own wallet, via Zebra's regtest **`generatetoaddress`** RPC,
  then waits for every sidecar to follow. This reproduces "each node mines its
  own coinbase" without any zcashd mining.
* Every other RPC passes straight through to the node's sidecar.

`generatetoaddress` was added to Zebra for this (regtest-only; it mines to a
caller-specified address instead of the configured `mining.miner_address`).

## Files

| File | Purpose |
| --- | --- |
| `harness.py` | The reusable harness: launches Zebra + N sidecars, mines per-node via Zebra, cleans up. |
| `wallet_conformance.py` | Single-node wallet conformance (address gen, coinbase maturity, balance, transparent spend, `getwalletinfo`). |
| `wallet_multinode.py` | Faithful 3-node port of `wallet.py`'s transparent self-mining assertions. |
| `run_upstream.py` | Adapter that runs an **unmodified** upstream test by monkeypatching the framework's node-lifecycle and topology primitives. |

## Requirements

```sh
export ZEBRAD_BIN=/path/to/zebrad          # built from this branch (has generatetoaddress + zcashd-compat)
export ZCASHD_BIN=/path/to/sidecar/zcashd  # a P2P-sidecar zcashd build (valargroup/zcashd)
```

## Running

Ported conformance tests (self-contained):

```sh
python3 wallet_conformance.py
python3 wallet_multinode.py
```

An unmodified upstream test, via the adapter:

```sh
export ZCASHD_RPC_TESTS_DIR=/path/to/zcash/qa/rpc-tests
python3 run_upstream.py wallet.py
```

## Coverage: what runs, and what can't

| Category | Status |
| --- | --- |
| Transparent + Sapling wallet tests | Run against the standard pairing. |
| Orchard / unified / accounts tests | Require a Regtest params file where **NU6.3 is not active** — the sidecar descopes Orchard from NU6.3, but supports it before activation, and Zebra supports it fully. |
| Miner tests (`getblocktemplate`, `submitblock`, `generate` on zcashd) | **Cannot pass** — those RPCs are removed from the sidecar by design; mining is Zebra's job. |
| `wallet_deprecation` (EOS halt) | Needs adapting — the sidecar removes the end-of-support halt. |

## Validation status

Built and validated in the Zebra tree:

* Zebra half end to end: regtest zebrad starts with the externally-managed
  zcashd-compat config, and both `generate` and the new `generatetoaddress`
  mine correctly (confirmed the coinbase pays the requested address, and that
  `generate` still uses the configured address).
* Harness Python: syntax-checked; the Zebra-facing paths exercised live.

Pending an environment that can execute the third-party sidecar binary:

* End-to-end runs of `wallet_conformance.py`, `wallet_multinode.py`, and
  `run_upstream.py wallet.py` against a live sidecar.
* Wiring the green run into the `zcashd-compat` CI job.

The sidecar↔Zebra P2P + wallet path these depend on is already covered by the
Rust integration suite (`zebrad/tests/common/zcashd_compat/`), which mines on
Zebra and reads wallet state back from the sidecar.
