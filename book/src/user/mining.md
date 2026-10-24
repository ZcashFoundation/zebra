# Mining Zcash with Zebra

Zebra supplies block templates and validates submitted blocks over JSON-RPC.
A mining pool distributes those templates to external Equihash solvers; Zebra
does not start a solver when an operator configures a reward address.

## Download Zebra

Install [Zebra](install.md) or use the [Docker instructions](mining-docker.md).
Miners must follow the rules at the candidate block's height, including the
changes in [NU7](#nu7-mining-rules); see the
[public activation schedule](../index.html#networks-and-network-upgrades).

## Configure Zebra for mining

Generate a configuration file:

```sh
zebrad generate -o zebrad.toml
```

Set the following fields in the existing sections. Replace the address
placeholder with an address controlled by the reward recipient.

```toml
[mining]
miner_address = "YOUR_MAINNET_MINING_ADDRESS"
internal_miner = false

[rpc]
listen_addr = "127.0.0.1:8232"
enable_cookie_auth = true
```

See the [mining configuration reference](config.md#mining) for field defaults
and limits.

### Miner address

The coinbase builder supports these address kinds on the selected network:

- Transparent pay-to-public-key-hash (P2PKH) and pay-to-script-hash (P2SH).
- Sapling payment addresses.
- [Unified Addresses](https://zips.z.cash/zip-0316) with a supported receiver.
  Zebra tries the Orchard receiver first, then Sapling, then transparent.

From NU6.3 onward, Zebra pays an Orchard receiver through the **Ironwood**
shielded pool, not the Orchard pool. Ironwood uses the same receiver address
format. Coinbase transactions cannot contain an Orchard component from
NU6.3 onward; see [ZIP 213](https://zips.z.cash/zip-0213) and
[ZIP 229](https://zips.z.cash/zip-0229).

Sprout addresses and
[transparent-source-only TEX addresses](https://zips.z.cash/zip-0320) are not
supported coinbase destinations. A syntactically valid address is not
sufficient: it must match the network and contain a supported receiver.
Zebra has no default reward recipient. Non-mining nodes need no address.
A missing address, a network mismatch, or an invalid memo prevents template
generation.

### Extra coinbase data

The coinbase transaction is the first transaction in a block. Its input
encodes the block height and arbitrary miner data under the
[transaction consensus rules](https://zips.z.cash/protocol/protocol.pdf#txnconsensus).
Zebra adds the Unicode U+1F993 marker, encoded as bytes `f0 9f a6 93`, to this
input. A nonempty `[mining] extra_coinbase_data` follows the marker and a
two-byte separator, a colon and a space (`3a 20`):

```toml
extra_coinbase_data = "/MyPoolName/"
```

The 86-byte limit reserves space for the height, marker, separator, and
script push encoding within the 100-byte coinbase input limit. Zebra rejects
an oversized value while loading the configuration. An unset or empty value
leaves only the marker after the height.

### Miner memo

Set `[mining] miner_memo` to attach a memo to a shielded miner output:

```toml
miner_memo = "Mined by MyPoolName"
```

A coinbase memo is **not private**. Shielded coinbase outputs must be
recoverable with the all-zero outgoing viewing key, so anyone can recover
their recipient, value, and memo. Do not include secrets. This differs from
ordinary shielded payments; see
[ZIP 213](https://zips.z.cash/zip-0213#specification).

### RPC section

Configure the pool to read Zebra's [RPC cookie](rpc.md#authentication) and
reload it after each restart. Bind RPC to loopback for a local pool; use a
protected tunnel or TLS proxy for remote connections. Disable authentication
only when an isolated network or an authenticating proxy controls access.

## Running Zebra

Start Zebra with the configuration file:

```sh
zebrad -c zebrad.toml start
```

Wait for synchronization before requesting mining work. On Mainnet,
`getblocktemplate` requires the sync service to be close to the tip and the
tip timestamp to be no more than 7,500 seconds old. A stale node or an
incorrect local clock can produce RPC error `-10`, indicating that Zebra
has not synced to the chain tip.

Testnet still requires synchronization, but it does not apply the Mainnet
tip-age limit. This permits mining to resume after a Testnet stall. Networks
with proof of work disabled, including default Regtest, bypass this sync
check. The `[rpc] debug_force_finished_sync` field changes only the estimate
in `getblockchaininfo`; it does not bypass the mining check.

## Testing the setup

On Linux, with the default cookie directory, request a template:

```sh
cookie="${XDG_CACHE_HOME:-$HOME/.cache}/zebra/.cookie"
curl --fail-with-body --silent --show-error \
  --user "$(cat "$cookie")" \
  --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblocktemplate","params":[]}' \
  http://127.0.0.1:8232/ | jq
```

Use the configured cookie path if `[rpc] cookie_dir` differs. A successful
response contains a `result` object with these mining inputs:

| Field | Meaning |
| --- | --- |
| `height`, `previousblockhash` | Candidate height and parent block. |
| `coinbasetxn.data` | Serialized coinbase transaction, including the configured reward destination. |
| `transactions` | Selected non-coinbase transactions; `depends` uses 1-based positions in this array. |
| `defaultroots` | Header commitment roots for the supplied coinbase and transactions. |
| `bits`, `target` | Compact and expanded difficulty targets. |
| `mintime`, `curtime`, `maxtime` | Unix timestamps, in seconds, for the allowed range and suggested block time. |
| `longpollid` | Opaque identifier for subsequent long-poll requests. |

Treat RPC errors as failures; an HTTP success status alone does not establish
that the node can generate templates.

## Mining RPCs

See the [mining RPC reference](rpc.md#mining) for method parameters and results.

Proposal mode accepts a request object with `"mode":"proposal"` and
`"data"` containing the candidate block hex. It checks the candidate without
requiring valid proof of work and returns `null` for a valid proposal or a
rejection string. Submit a solved block with `submitblock`, not with a
`getblocktemplate` submit mode.

A `submitblock` result of `null` means Zebra committed the block, including
when it is on a side chain. Unlike zcashd's distinction between accepted and
inconclusive submissions, this result does not prove that the block is on
Zebra's best chain.

### Long polling and template expiry

Long polling keeps an RPC request open until work changes. Pass the previous
template's `longpollid` in the next request object:

```json
{"jsonrpc":"2.0","id":2,"method":"getblocktemplate","params":[{"longpollid":"REPLACE_WITH_RETURNED_LONGPOLLID"}]}
```

Zebra wakes the request when the tip or relevant template state changes. It
also returns when the template's inclusive `maxtime` expires during the
request, even if the tip and mempool do not change. The response can retain
the same `longpollid` for this expiry notification. Repeating a request for
that already-expired template waits for changed work rather than repeatedly
returning the same expiry notification.

The response's `submitold` field is false when the tip changes or the time
range expires. Discard old work in that case. A mempool-only change can return
`submitold: true`. Allow long requests through the pool's HTTP client and
any reverse proxy; do not interpret a pending long poll as an RPC failure.

## Coinbase and consensus rules

Use the supplied coinbase and transaction set unless the pool implements the
current consensus rules. Modifying transactions changes fees and commitment
roots; modifying the coinbase also requires recomputing the affected roots.
The `defaultroots` values apply only to the template as returned.

The template marks the coinbase as required and advertises only `"time"` in
its `mutable` list. Request a fresh template when changing the parent or the
transaction set rather than reusing its coinbase.

Zebra constructs mandatory funding-stream outputs and scheduled lockbox
disbursements. The lockbox is the deferred value pool: its allocation is
accounted for without an ordinary recipient output. Under the active
revision-2 funding streams, 12% of the subsidy is deferred and 8% goes to
Zcash Community Grants through the Financial Privacy Fund; the miner receives
the remainder and claimable fees. Amounts use consensus integer rounding.
See [ZIP 207](https://zips.z.cash/zip-0207),
[ZIP 214](https://zips.z.cash/zip-0214), and
[ZIP 271](https://zips.z.cash/zip-0271).

From NU6 onward, a coinbase must claim its full allowed value; miners cannot
silently discard a portion of the subsidy or fees. Preserve the deferred
allocation and any disbursement as well as the miner output. See
[ZIP 236](https://zips.z.cash/zip-0236).

Transparent coinbase outputs must mature for 100 blocks and must be shielded
when spent on public networks. Shielded coinbase outputs do not have that
100-block maturity rule. See
[ZIP 213](https://zips.z.cash/zip-0213#specification) and the
[transaction consensus rules](https://zips.z.cash/protocol/protocol.pdf#txnconsensus).

From NU6.3 (Ironwood) activation, the template's coinbase is a version 6
transaction, and the template can include version 6 transactions from the
mempool. Pool software that parses, checks, or rebuilds template transactions
must support the [version 6 transaction format](https://zips.z.cash/zip-0229).

### NU7 mining rules

Zebra 7.0.0 applies these rules from the NU7 activation height on each network:

- **Timing and issuance:** [ZIP 218](https://zips.z.cash/zip-0218) changes the
  target spacing from 75 to 25 seconds and the difficulty averaging window
  from 17 to 102 blocks. Scheduled subsidy per block is divided by three,
  with integer rounding, and remaining halving intervals are tripled.
- **Block contents:** ZIP 218 limits a block to 330 Orchard actions, 330
  Ironwood actions, and 300 Sapling inputs plus outputs, with a shared budget
  of 330 across those pools. The limits include coinbase outputs. Sprout
  JoinSplits are disallowed. [ZIP 2003](https://zips.z.cash/zip-2003) disallows
  version 4 transactions; versions 5 and 6 remain valid.
- **Fees:** [ZIP 235](https://zips.z.cash/zip-0235) removes
  `floor(total_transaction_fees * 6 / 10)` zatoshis from circulation into the
  Network Sustainability Mechanism (NSM) reserve. Calculate this once over
  the block's aggregate fees, not separately for each transaction. The
  coinbase claims only the remainder. The template's `coinbasetxn.fee` is
  the negative of those claimable miner fees; non-coinbase `fee` fields
  remain gross.
- **Reserve reissuance:** [ZIP 237](https://zips.z.cash/zip-0237) adds a
  rounded-up fraction of the parent block's NSM reserve to scheduled subsidy
  from height **8,939,664 on Mainnet** or **7,305,222 on Testnet**. This is not
  immediate reissuance at NU7 activation. From that height,
  `getblocksubsidy` needs the parent block's state and cannot calculate
  subsidies beyond the block after the best tip.
- **Funding periods:** ZIPs 207 and 214 stretch the remaining funding periods
  for the faster spacing. The revision-2 streams end at the adjusted third
  halving: **6,132,390 on Mainnet** and **4,497,948 on Testnet**, exclusive.
  [ZIP 2008](https://zips.z.cash/zip-2008) rotates the Mainnet grants recipient
  starting with the first funding period after the period containing the
  last pre-NU7 block. Do not hard-code a single recipient address in a pool.

The NSM reserve is separate from the lockbox. The former tracks funds removed
from circulating supply for algorithmic reissuance; the latter holds deferred
funding-stream value. Zebra's generated coinbase applies both mechanisms.

## Run a mining pool

Configure the pool's daemon connection with Zebra's RPC address and
credentials. Confirm that the pool handles the supplied transaction formats,
coinbase, commitment roots, long polling, and NU7 fee split. Pool accounting
and payments to individual workers are the pool's responsibility, not
Zebra's.

The [s-nomp Testnet setup](mining-testnet-s-nomp.md) is experimental.

## Testnet, Regtest, and the internal miner

For public Testnet, set `[network] network = "Testnet"`, use a Testnet reward
address, and set `[rpc] listen_addr = "127.0.0.1:18232"`. Public Testnet uses
real Equihash proof of work and the same upgrade-dependent coinbase rules.

For deterministic local block production, use the
[Regtest generation RPCs](regtest.md#using-rpc-methods-directly), not the
[experimental internal miner](regtest.md#using-zebras-internal-miner).
