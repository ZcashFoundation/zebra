# JSON-RPC server

Zebra serves chain data, transaction submission, and mining operations over
HTTP JSON-RPC. Zebra does not hold wallet keys or implement wallet accounting;
[Zallet](zallet.md) provides the wallet API. The
[zcashd API migration guide](zcashd-api-migration-guide.md) records differences
from `zcashd`, including missing fields and unsupported methods.

## Enabling the server

JSON-RPC is disabled until [`[rpc] listen_addr`](config.md#rpc) is set to an
explicit IP address and port. See [Ports](run.md#ports) for network conventions.

Generate a configuration file:

```sh
zebrad generate -o zebrad.toml
```

Add the listener to its existing `[rpc]` section:

```toml
[rpc]
listen_addr = "127.0.0.1:8232"
```

Start Zebra with that file:

```sh
zebrad -c zebrad.toml start
```

Keep the listener on loopback. Zebra provides neither TLS encryption nor
per-method access control. A client with RPC access can submit transactions
and blocks and call state-changing methods such as `invalidateblock`.
Cookie authentication authenticates the caller; it does not encrypt traffic.
For remote access, use an authenticated, TLS-terminating reverse proxy or an
encrypted tunnel, and prevent clients from reaching the plaintext listener
directly. Do not publish an unauthenticated RPC port to the internet.

## Authentication

The `[rpc] enable_cookie_auth` field defaults to `true`. At JSON-RPC server
startup, Zebra generates a random credential and writes
`<cookie_dir>/.cookie` in this format:

```text
__cookie__:<base64-encoded random secret>
```

The username is `__cookie__`; the secret encodes 32 random bytes. Zebra creates
new cookie files with mode `0600` on Unix and refuses to write through a
symlink. Restrict access to the cookie directory to the node operator and
trusted clients. Treat the file as a password, not as an HTTP browser cookie.
Clients send its contents using HTTP Basic authentication and must read the
new credential after a Zebra restart.
On clean RPC shutdown, Zebra removes the cookie file; removal failures are
logged.

The [`[rpc] cookie_dir`](config.md#rpc) setting is independent of
`[state] cache_dir`: changing the state directory does not move the cookie.
Give each Zebra instance a separate cookie directory, even on different networks.

For the Linux default cookie directory, call the server with:

```sh
cookie="${XDG_CACHE_HOME:-$HOME/.cache}/zebra/.cookie"
curl --user "$(cat "$cookie")" \
  --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}' \
  http://127.0.0.1:8232/
```

Set `cookie` to the configured directory's `.cookie` file when using a custom
`[rpc] cookie_dir`.

Zebra has no configuration for static RPC usernames and passwords. Setting
`[rpc] enable_cookie_auth = false` disables credential checks; credentials
sent by clients then have no effect. This is required by
[lightwalletd](lightwalletd.md#json-rpc), which does not read Zebra's cookie.
Authentication settings apply only to JSON-RPC, not to either gRPC listener.

## Configuration

See the [configuration reference](config.md#rpc) for `[rpc]` fields and defaults.
The experimental lightwalletd-compatible gRPC service is covered in
[Serving light clients directly from Zebra](lightwalletd.md#experimental-serving-light-clients-directly-from-zebra).

Zebra has no `[rpc]` CORS, allowed-origin, client-IP allowlist, or batch-count
configuration. Cross-origin browser restrictions are not a replacement for
RPC authentication or network access controls.

### Indexer gRPC service

The indexer service is separate from JSON-RPC and from the wallet-facing
`CompactTxStreamer` protocol. Its
[protocol definition](https://github.com/ZcashFoundation/zebra/blob/main/zebra-rpc/proto/indexer.proto)
provides four methods:

| Method | Purpose |
| --- | --- |
| `ChainTipChange` | Streams best-chain tip hashes and heights. |
| `NonFinalizedStateChange` | Streams non-finalized blocks after the caller's known tips. |
| `MempoolChange` | Streams transaction additions, invalidations, and mining events. |
| `GetBlock` | Returns a best-chain block by hash or height. |

Indexers use these streams alongside read-only access to Zebra's finalized
state database. Build with the [`indexer` feature](install.md#cargo-features)
for consumers that require the spending-transaction index, including
[Zallet's Zebra backend](zallet.md). The listener does not require this feature:
successful startup does not establish that the index exists.

Set `[rpc] indexer_listen_addr = "127.0.0.1:8230"` to enable the service on an
example port. It uses plaintext HTTP/2 without authentication; JSON-RPC cookie
settings do not protect it. Keep it on loopback or a restricted private network.

## Transport and compatibility

Send HTTP POST requests with `Content-Type: application/json`. Zebra accepts
JSON-RPC 2.0 and the legacy request format used by `zcashd`. For individual
legacy requests, include `method`, `params`, and `id`, and omit `jsonrpc`.
Zebra also accepts lightwalletd's `"jsonrpc":"1.0"` field. The compatibility
middleware converts the response back to the corresponding legacy format,
including both `result` and `error`. Missing content types and `text/plain`
are treated as JSON. The server does not offer WebSocket transport.

Use JSON-RPC 2.0 for batches. Send an array of request objects, each with its
own `"jsonrpc":"2.0"` and `id`. The legacy compatibility conversion applies
to individual objects, not to an entire batch. Batches have no separate
request-count limit, but the request body is limited to 4,001,024 bytes and
the response body to `[rpc] max_response_body_size`.

For example, using the cookie variable from [Authentication](#authentication):

```sh
curl --user "$(cat "$cookie")" \
  --header 'Content-Type: application/json' \
  --data '[{"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]},{"jsonrpc":"2.0","id":2,"method":"getbestblockhash","params":[]}]' \
  http://127.0.0.1:8232/
```

Zebra uses `zcashd`-style application error codes, including `-5` for an
unknown block hash and `-8` for an invalid block height. The RPC middleware
rewrites invalid-parameter code `-32602` to `-1` while preserving the error
message, data, and request ID. Other error codes pass through unchanged;
this is not a guarantee that every failure matches `zcashd` byte for byte.

## Methods

Zebra 7.0.0 registers the following 41 JSON-RPC method names. The
[RPC trait and implementation](https://github.com/ZcashFoundation/zebra/blob/main/zebra-rpc/src/methods.rs)
define their parameters and results. The `rpc.discover` method returns an
OpenRPC 1.3.2 document with machine-readable method and type schemas.

### Control and discovery

| Method | Semantics |
| --- | --- |
| `getinfo` | Returns node software, chain, connection, and status information. |
| `getdeprecationinfo` | Returns end-of-support information, with an estimated halt height on Mainnet. |
| `rpc.discover` | Returns the server's OpenRPC schema. |
| `stop` | Requests process shutdown on Regtest; unavailable on other networks and on Windows. |

### Blockchain

| Method | Semantics |
| --- | --- |
| `getblockchaininfo` | Returns chain height, network upgrades, value pools, and synchronization estimates. |
| `getbestblockhash` | Returns the best-chain tip hash. |
| `getbestblockheightandhash` | Returns the best-chain tip height and hash together. |
| `getblockcount` | Returns the best-chain tip height. |
| `getblockhash` | Returns the block hash at a height; negative indices count back from the tip. |
| `getblock` | Returns a block by hash or height as hexadecimal data or a structured response with verbosity 1, 2, or 3. |
| `getblockheader` | Returns a block header by hash or height as hexadecimal data or a structured response. |
| `getdifficulty` | Returns proof-of-work difficulty relative to the minimum difficulty. |
| `gettxout` | Returns an unspent transparent output, optionally accounting for the mempool. |
| `z_gettreestate` | Returns Sapling, Orchard, and Ironwood shielded tree state at a block, according to pool activation. |
| `z_getsubtreesbyindex` | Returns indexed subtree roots and completion heights for Sapling, Orchard, or Ironwood. |
| `invalidateblock` | Invalidates a block in non-finalized state; cannot invalidate finalized blocks. |
| `reconsiderblock` | Reconsiders a cached invalidated block and returns the reconsidered block hashes. |

### Transactions and mempool

The mempool holds verified transactions that have not yet been mined.

| Method | Semantics |
| --- | --- |
| `getmempoolinfo` | Returns aggregate mempool size and memory information. |
| `getrawmempool` | Returns mempool transaction IDs, or transaction metadata with verbose output. |
| `getrawtransaction` | Returns a transaction as hexadecimal data or a structured response, optionally restricted to a block. |
| `sendrawtransaction` | Submits a signed transaction to the mempool and returns its transaction ID; `allowhighfees` is accepted but ignored. |
| `getstandardfee` | Returns the recommended fee per logical action in zatoshis, with `version: 0`. For the next block on Mainnet, reports 5,000 before height 3,590,000 and 1,000 from that height; test networks report 1,000 at every height. |

### Transparent address index

| Method | Semantics |
| --- | --- |
| `getaddressbalance` | Returns confirmed balance and received amount for transparent addresses. |
| `getaddresstxids` | Returns transaction IDs involving transparent addresses within an optional height range. |
| `getaddressutxos` | Returns unspent outputs for transparent addresses, with optional height, count, and chain-information fields. |

### Mining

See [Mining](mining.md) for configuration and block-template use.

| Method | Semantics |
| --- | --- |
| `getblocktemplate` | Takes an optional request object; returns a mining template, supports long polling, or validates a proposed block. |
| `submitblock` | Validates and submits a hexadecimal block; an optional work-ID object is ignored. Returns `null`, `"duplicate"`, or `"rejected"`, or an RPC error. See the [mining guide](mining.md) for result caveats. |
| `getmininginfo` | Takes no parameters; returns tip height, block size and non-coinbase transaction count when available, network name, and estimated network solution rate. |
| `getblocksubsidy` | Returns miner subsidy, funding-stream and lockbox allocations excluding transaction fees at an optional height (default: tip); reserve-dependent future rewards require the preceding block. |
| `getnetworksolps` | Estimates network solutions per second with optional block count and height (defaults: 120 blocks, tip). A nonpositive count selects the [difficulty averaging window](https://zips.z.cash/zip-0218) at the effective query height: 17 blocks before NU7, 102 from NU7. |
| `getnetworkhashps` | Deprecated alias for `getnetworksolps`. |
| `generate` | Generates blocks paying the configured miner address when proof of work is disabled, including Regtest. |
| `generatetoaddress` | Generates blocks paying a supplied address when proof of work is disabled. |

### Peer networking

| Method | Semantics |
| --- | --- |
| `getnetworkinfo` | Returns network protocol, connection, and relay information. |
| `getpeerinfo` | Returns the peer information tracked by Zebra's address book. |
| `ping` | Returns success without initiating a peer ping. |
| `addnode` | Adds a peer address on Regtest; only the `add` command is supported. |

### Address utilities

A [Unified Address](https://zips.z.cash/zip-0316) contains one or more payment
receivers. A [TEX address](https://zips.z.cash/zip-0320) encodes a transparent
receiver with a restriction on the source of its funds.

| Method | Semantics |
| --- | --- |
| `validateaddress` | Validates a transparent address and returns its supported metadata. |
| `z_validateaddress` | Validates a supported Zcash address and reports its address type and metadata. |
| `z_converttex` | Converts a transparent P2PKH address to a TEX address. |
| `z_listunifiedreceivers` | Extracts supported payment receivers from a Unified Address. |
