# zcashd API Migration Guide

`zcashd` is retired. For every public API method it exposes, an operator
migrating away from it needs one question answered: **who owns this now?**

This page is that record. For each of the 135 RPC methods `zcashd` 6.20.0
registers, plus its ZMQ topics, REST interface, and notification hooks, it
names the component that owns the API method now, the method that replaces it,
or the reason it was retired.

It is a statement of ownership, not a feature comparison. "Zebra owns
`getblockchaininfo`" means Zebra is where that API method lives from now on; it
does not promise byte-identical responses. Where Zebra diverges from
`zcashd`, the Notes column says how.

This page describes Zebra 7.0.0-rc.0 and Zallet 0.1.0-beta.3. Zallet releases after
0.1.0-beta.3 read chain data only from Zebra; see
[Running Zallet with Zebra](zallet.md) for what that requires of `zebrad`.

## Where the boundary falls today

| Outcome                        | Methods | Share |
| ------------------------------ | ------- | ----- |
| **Available from Zebra**       | 36      | 27%   |
| **Available from Zallet**      | 27      | 20%   |
| **Replaced** by another method | 21      | 16%   |
| **Planned**                    | 18      | 13%   |
| **Deferred**                   | 6       | 4%    |
| **Undecided**                  | 2       | 1%    |
| **Retired**                    | 25      | 19%   |

84 of the 135 methods (62%) have a working path today: the method itself, or a
replacement that does the same job.

## How to read the table

### Method

Links go to the [`zcashd` RPC reference](https://zcash.github.io/rpc/).
Methods the generated reference omits are unlinked: the hidden methods, and the
wallet-encryption methods, which `zcashd` only lists in `help` for an encrypted
wallet.

### Owner

Who owns the API method now that `zcashd` is gone.

| Owner      | Meaning                                                                                                                                                                                                                                                                         |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Zebra**  | The validator node. Node-level chain, network, mempool, and mining API methods.                                                                                                                                                                                                 |
| **Zallet** | The [`zcashd` wallet replacement](https://github.com/zcash/wallet). Anything that needs keys or wallet state. Per-method status follows Zallet's changelog; its [JSON-RPC method status](https://zcash.github.io/zallet/zcashd/rpc_status.html) page lags it for a few methods. |
| **—**      | No owner: the method is retired, or its disposition is still open.                                                                                                                                                                                                              |
|  |

Owner names the component that _implements_ the API method. Zallet also serves
a few node-level methods, such as `getrawtransaction`, `validateaddress`, and
`z_listunifiedreceivers`; where Zebra implements the method too, the row says
**Zebra**.

A marker on the owner records what the [`zcashd`-compat
sidecar](zcashd-compat.md) does with the method during a migration. The sidecar
is a **temporary bridge, not an owner**: an unmarked row buys migration time, it
does not remove the need to move to the owner named in the column.

| Marker   | Meaning                                                                                                                                                                                          |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| _(none)_ | The sidecar serves it with stock `zcashd` semantics.                                                                                                                                             |
| **†**    | The sidecar serves it but rejects some inputs — Ironwood always, and Orchard from NU6.3 on. See [Wallet shielded-pool support](zcashd-compat.md#wallet-shielded-pool-support-orchard--ironwood). |
| **\***   | `zcashd` registers it, but the sidecar build does not. The miner RPCs and `addnode` are removed so a misconfigured miner or peer setup fails loudly.                                             |

### Status

Whether the API method is available **from the owner named in the previous
column** — not from `zcashd`.

| Status        | Meaning                                                                              |
| ------------- | ------------------------------------------------------------------------------------ |
| **Done**      | The owner implements it and treats it as a supported API method.                     |
| **Partial**   | Implemented, but diverges from `zcashd`. The Notes column says how.                  |
| **Stub**      | Registered and returns success, but does nothing.                                    |
| **Replaced**  | The owner provides a different method or mechanism for the same job; Notes names it. |
| **Planned**   | The owner has decided to provide it; not shipped yet.                                |
| **Deferred**  | Owner assigned; waits on other work named in Notes.                                  |
| **Undecided** | Whether the owner will provide it is still open.                                     |
| **Retired**   | Not provided by anyone; it goes away with `zcashd`. Notes says why.                  |

> [!NOTE]
> The table might be outdated. If a method is listed as Planned, Deferred, or
> Undecided and you need it, check the owner's current release before concluding
> it is unavailable.

### Decisions and open questions

The dispositions of the `zcashd` node and utility methods Zebra does not
implement are recorded in [#11022][11022-decisions]. Where an older issue
decided a row, the Notes column links it. Open questions are tracked in:

- [#11033](https://github.com/ZcashFoundation/zebra/issues/11033): unresolved
  differences in methods Zebra does implement.
- [#11027](https://github.com/ZcashFoundation/zebra/issues/11027): peer
  topology and relay-policy controls.
- [#11281](https://github.com/ZcashFoundation/zebra/issues/11281): the v2 P2P
  protocol, which the deferred peer-control methods wait on.

### Keeping this page current

Any PR that adds, removes, or intentionally changes a `zcashd`-compatible
method, field, notification, or integration point updates this page in the
same PR. Before each Zebra stable release, the page is checked against the
owners' current releases and the version line at the top is updated.

## RPC methods

### Control

| Method                                                                                | Owner                 | Status   | Notes                                                                                                |
| ------------------------------------------------------------------------------------- | --------------------- | -------- | ---------------------------------------------------------------------------------------------------- |
| [`getexperimentalfeatures`](https://zcash.github.io/rpc/getexperimentalfeatures.html) | —                     | Retired  | Out of scope for a validator node.                                                                   |
| [`getinfo`](https://zcash.github.io/rpc/getinfo.html)                                 | Zebra                 | Partial  | No `timeoffset`. The wallet fields are zcashd-wallet-only and never apply.                           |
| [`getmemoryinfo`](https://zcash.github.io/rpc/getmemoryinfo.html)                     | —                     | Retired  | Out of scope for a validator node.                                                                   |
| [`help`](https://zcash.github.io/rpc/help.html)                                       | [Zallet][zallet-help] | Done     | Zallet serves `help` for its own RPC; Zebra does not.                                                |
| [`setlogfilter`](https://zcash.github.io/rpc/setlogfilter.html)                       | Zebra                 | Replaced | Use the tracing `/filter` endpoint (`filter-reload` build feature); see [Tracing Zebra](tracing.md). |
| [`stop`](https://zcash.github.io/rpc/stop.html)                                       | Zebra                 | Partial  | Regtest only. Returns `Zebra server stopping`, not zcashd's `Zcash server stopping`. [#11033]        |

### Blockchain

| Method                                                                          | Owner | Status  | Notes                                                                                                                                                                                                                                                                                                                                      |
| ------------------------------------------------------------------------------- | ----- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [`getbestblockhash`](https://zcash.github.io/rpc/getbestblockhash.html)         | Zebra | Done    |                                                                                                                                                                                                                                                                                                                                            |
| [`getblock`](https://zcash.github.io/rpc/getblock.html)                         | Zebra | Partial | No `authdataroot`, `chainhistoryroot`, `chainwork`, or `anchor`. The `verbosity` argument must be a number, where zcashd also accepts a bool. Adds `nTx` and an `ironwood` entry in `trees`. Also accepts verbosity `3`, which zcashd rejects: it adds a `prevout` to each transparent input and a `fee` to each non-coinbase transaction. |
| [`getblockchaininfo`](https://zcash.github.io/rpc/getblockchaininfo.html)       | Zebra | Partial | No `initial_block_download_complete` or `softforks`. Some fields are placeholders. [#11033]                                                                                                                                                                                                                                                |
| [`getblockcount`](https://zcash.github.io/rpc/getblockcount.html)               | Zebra | Done    |                                                                                                                                                                                                                                                                                                                                            |
| [`getblockdeltas`](https://zcash.github.io/rpc/getblockdeltas.html)             | —     | Retired | Block-explorer index, out of scope for a validator node. Build on the indexer gRPC service instead.                                                                                                                                                                                                                                        |
| [`getblockhash`](https://zcash.github.io/rpc/getblockhash.html)                 | Zebra | Done    |                                                                                                                                                                                                                                                                                                                                            |
| [`getblockhashes`](https://zcash.github.io/rpc/getblockhashes.html)             | —     | Retired | Block-explorer index, out of scope for a validator node. Build on the indexer gRPC service instead. [#8436]                                                                                                                                                                                                                                |
| [`getblockheader`](https://zcash.github.io/rpc/getblockheader.html)             | Zebra | Partial | No `chainwork`, which is undocumented in zcashd. Adds `blockcommitments`.                                                                                                                                                                                                                                                                  |
| [`getchaintips`](https://zcash.github.io/rpc/getchaintips.html)                 | Zebra | Planned | [#10666]                                                                                                                                                                                                                                                                                                                                   |
| [`getdifficulty`](https://zcash.github.io/rpc/getdifficulty.html)               | Zebra | Partial | Computed from the high 128 bits of the expanded difficulty instead of zcashd's `f64` division; the two agree to `f64` precision. Errors where zcashd returns `1.0` on a chain too short to measure.                                                                                                                                        |
| [`getmempoolinfo`](https://zcash.github.io/rpc/getmempoolinfo.html)             | Zebra | Partial | The regtest-only key is spelled `fully_notified`; zcashd spells it `fullyNotified`.                                                                                                                                                                                                                                                        |
| [`getrawmempool`](https://zcash.github.io/rpc/getrawmempool.html)               | Zebra | Done    |                                                                                                                                                                                                                                                                                                                                            |
| [`getspentinfo`](https://zcash.github.io/rpc/getspentinfo.html)                 | Zebra | Planned | Will use the spending-transaction index from Zebra's `indexer` build feature.                                                                                                                                                                                                                                                              |
| [`gettxout`](https://zcash.github.io/rpc/gettxout.html)                         | Zebra | Done    |                                                                                                                                                                                                                                                                                                                                            |
| [`gettxoutproof`](https://zcash.github.io/rpc/gettxoutproof.html)               | Zebra | Planned | [Decision][11022-decisions]                                                                                                                                                                                                                                                                                                                |
| [`gettxoutsetinfo`](https://zcash.github.io/rpc/gettxoutsetinfo.html)           | —     | Retired | Block-explorer index, out of scope for a validator node. Build on the indexer gRPC service instead.                                                                                                                                                                                                                                        |
| [`verifychain`](https://zcash.github.io/rpc/verifychain.html)                   | —     | Retired | Out of scope for a validator node.                                                                                                                                                                                                                                                                                                         |
| [`verifytxoutproof`](https://zcash.github.io/rpc/verifytxoutproof.html)         | Zebra | Planned | [Decision][11022-decisions]                                                                                                                                                                                                                                                                                                                |
| [`z_getsubtreesbyindex`](https://zcash.github.io/rpc/z_getsubtreesbyindex.html) | Zebra | Done    |                                                                                                                                                                                                                                                                                                                                            |
| [`z_gettreestate`](https://zcash.github.io/rpc/z_gettreestate.html)             | Zebra | Partial | Each pool reports `finalState` only — no `finalRoot` or `skipHash`. `sprout` is omitted when empty, where zcashd always emits it. Adds `ironwood`. [#11033]                                                                                                                                                                                |

### Address index

| Method                                                                    | Owner | Status  | Notes                                                                                                       |
| ------------------------------------------------------------------------- | ----- | ------- | ----------------------------------------------------------------------------------------------------------- |
| [`getaddressbalance`](https://zcash.github.io/rpc/getaddressbalance.html) | Zebra | Done    |                                                                                                             |
| [`getaddressdeltas`](https://zcash.github.io/rpc/getaddressdeltas.html)   | —     | Retired | Block-explorer index, out of scope for a validator node. Build on the indexer gRPC service instead. [#8440] |
| [`getaddressmempool`](https://zcash.github.io/rpc/getaddressmempool.html) | —     | Retired | Block-explorer index, out of scope for a validator node. Build on the indexer gRPC service instead.         |
| [`getaddresstxids`](https://zcash.github.io/rpc/getaddresstxids.html)     | Zebra | Done    |                                                                                                             |
| [`getaddressutxos`](https://zcash.github.io/rpc/getaddressutxos.html)     | Zebra | Done    |                                                                                                             |

### Mining

| Method                                                                            | Owner   | Status  | Notes                                                                                                                                                                                                    |
| --------------------------------------------------------------------------------- | ------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`getblocksubsidy`](https://zcash.github.io/rpc/getblocksubsidy.html)             | Zebra   | Done    |                                                                                                                                                                                                          |
| [`getblocktemplate`](https://zcash.github.io/rpc/getblocktemplate.html)           | Zebra\* | Done    | Superset: adds `maxtime` and `submitold`. `capabilities` and `mutable` match zcashd, and long polling and proposal mode are both implemented.                                                            |
| [`getlocalsolps`](https://zcash.github.io/rpc/getlocalsolps.html)                 | —       | Retired | Out of scope for a validator node.                                                                                                                                                                       |
| [`getmininginfo`](https://zcash.github.io/rpc/getmininginfo.html)                 | Zebra   | Partial | No `difficulty`, `errors`, `errorstimestamp`, `genproclimit`, `localsolps`, `pooledtx`, or `generate`.                                                                                                   |
| [`getnetworkhashps`](https://zcash.github.io/rpc/getnetworkhashps.html)           | Zebra   | Done    | Deprecated alias for `getnetworksolps`, in both zcashd and Zebra.                                                                                                                                        |
| [`getnetworksolps`](https://zcash.github.io/rpc/getnetworksolps.html)             | Zebra   | Done    |                                                                                                                                                                                                          |
| [`prioritisetransaction`](https://zcash.github.io/rpc/prioritisetransaction.html) | Zebra   | Planned | [Decision][11022-decisions]                                                                                                                                                                              |
| [`submitblock`](https://zcash.github.io/rpc/submitblock.html)                     | Zebra\* | Partial | Returns only `null`, `duplicate`, or `rejected`. zcashd also returns `duplicate-invalid`, `duplicate-inconclusive`, `inconclusive`, and the specific BIP-22 reject reason in place of a bare `rejected`. |

### Generating

| Method                                                        | Owner   | Status  | Notes                                                                                 |
| ------------------------------------------------------------- | ------- | ------- | ------------------------------------------------------------------------------------- |
| [`generate`](https://zcash.github.io/rpc/generate.html)       | Zebra\* | Done    | Regtest only, as in zcashd.                                                           |
| [`getgenerate`](https://zcash.github.io/rpc/getgenerate.html) | —\*     | Retired | Controls zcashd's built-in CPU miner. Mine with `getblocktemplate` and `submitblock`. |
| [`setgenerate`](https://zcash.github.io/rpc/setgenerate.html) | —\*     | Retired | Controls zcashd's built-in CPU miner. Mine with `getblocktemplate` and `submitblock`. |

### Network

| Method                                                                      | Owner   | Status   | Notes                                                                                                                                                           |
| --------------------------------------------------------------------------- | ------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`addnode`](https://zcash.github.io/rpc/addnode.html)                       | Zebra\* | Partial  | Regtest only, and only the `add` command. [#10329]                                                                                                              |
| [`clearbanned`](https://zcash.github.io/rpc/clearbanned.html)               | Zebra   | Deferred | Deferred to the v2 P2P protocol ([#11281]).                                                                                                                     |
| [`disconnectnode`](https://zcash.github.io/rpc/disconnectnode.html)         | Zebra   | Deferred | Deferred to the v2 P2P protocol ([#11281]).                                                                                                                     |
| [`getaddednodeinfo`](https://zcash.github.io/rpc/getaddednodeinfo.html)     | Zebra   | Deferred | Deferred to the v2 P2P protocol ([#11281]).                                                                                                                     |
| [`getconnectioncount`](https://zcash.github.io/rpc/getconnectioncount.html) | Zebra   | Replaced | Use `connections` from `getnetworkinfo` or `getinfo`.                                                                                                           |
| [`getdeprecationinfo`](https://zcash.github.io/rpc/getdeprecationinfo.html) | Zebra   | Partial  | Only `end_of_service`. No `version`, `subversion`, `deprecated_features`, or `disabled_features`.                                                               |
| [`getnettotals`](https://zcash.github.io/rpc/getnettotals.html)             | Zebra   | Deferred | Deferred to the v2 P2P protocol ([#11281]).                                                                                                                     |
| [`getnetworkinfo`](https://zcash.github.io/rpc/getnetworkinfo.html)         | Zebra   | Partial  | No `warningstimestamp`. `localaddresses` and `warnings` are empty. [#11030]                                                                                     |
| [`getpeerinfo`](https://zcash.github.io/rpc/getpeerinfo.html)               | Zebra   | Partial  | 10 of zcashd's 24 per-peer fields; Zebra's peer set does not track the rest. Adds `connection_state`.                                                           |
| [`listbanned`](https://zcash.github.io/rpc/listbanned.html)                 | Zebra   | Deferred | Deferred to the v2 P2P protocol ([#11281]).                                                                                                                     |
| [`ping`](https://zcash.github.io/rpc/ping.html)                             | Zebra   | Stub     | Intentional ([#11028]): Zebra already pings every peer about once a minute, so `getpeerinfo`'s `pingtime` and `pingwait` stay current without an explicit ping. |
| [`setban`](https://zcash.github.io/rpc/setban.html)                         | Zebra   | Deferred | Deferred to the v2 P2P protocol ([#11281]).                                                                                                                     |

### Raw transactions

| Method                                                                          | Owner                                 | Status   | Notes                                                                                                                                                                                |
| ------------------------------------------------------------------------------- | ------------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [`createrawtransaction`](https://zcash.github.io/rpc/createrawtransaction.html) | Zallet                                | Replaced | Use Zallet's PCZT methods (`pczt_create`, `pczt_prove`, `pczt_sign`, `pczt_extract`) instead. [#8952]                                                                                |
| [`decoderawtransaction`](https://zcash.github.io/rpc/decoderawtransaction.html) | [Zallet][zallet-decoderawtransaction] | Done     | Not implemented in Zebra. [#8645]                                                                                                                                                    |
| [`decodescript`](https://zcash.github.io/rpc/decodescript.html)                 | [Zallet][zallet-decodescript]         | Done     |                                                                                                                                                                                      |
| [`fundrawtransaction`](https://zcash.github.io/rpc/fundrawtransaction.html)     | Zallet                                | Replaced | Use Zallet's PCZT methods instead.                                                                                                                                                   |
| [`getrawtransaction`](https://zcash.github.io/rpc/getrawtransaction.html)       | Zebra                                 | Partial  | `vout` omits `valueSat`, and the `-spentindex` fields (`spentTxId`, `spentIndex`, `spentHeight`) are absent. P2PK outputs have an empty `addresses` list ([#9671]). Adds `ironwood`. |
| [`sendrawtransaction`](https://zcash.github.io/rpc/sendrawtransaction.html)     | Zebra                                 | Partial  | `allowhighfees` is accepted and ignored.                                                                                                                                             |
| [`signrawtransaction`](https://zcash.github.io/rpc/signrawtransaction.html)     | Zallet                                | Replaced | Use Zallet's PCZT methods instead.                                                                                                                                                   |

### Util

| Method                                                                    | Owner                          | Status  | Notes                                                                                                                                                                                              |
| ------------------------------------------------------------------------- | ------------------------------ | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`createmultisig`](https://zcash.github.io/rpc/createmultisig.html)       | [Zallet][zallet-status]        | Planned |                                                                                                                                                                                                    |
| [`validateaddress`](https://zcash.github.io/rpc/validateaddress.html)     | Zebra                          | Partial | No `scriptPubKey`. Adds `isscript`.                                                                                                                                                                |
| [`verifymessage`](https://zcash.github.io/rpc/verifymessage.html)         | [Zallet][zallet-verifymessage] | Done    | Not implemented in Zebra. [#8947]                                                                                                                                                                  |
| [`z_validateaddress`](https://zcash.github.io/rpc/z_validateaddress.html) | Zebra                          | Partial | Emits `address_type` but not zcashd's deprecated `type` alias. No `diversifier` or `diversifiedtransmissionkey` (Sapling), or `payingkey` or `transmissionkey` (Sprout). `ismine` is always false. |

### Wallet

| Method                                                                                    | Owner                                   | Status    | Notes                                                                                                                                          |
| ----------------------------------------------------------------------------------------- | --------------------------------------- | --------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| [`addmultisigaddress`](https://zcash.github.io/rpc/addmultisigaddress.html)               | [Zallet][zallet-status]                 | Planned   |                                                                                                                                                |
| [`backupwallet`](https://zcash.github.io/rpc/backupwallet.html)                           | —                                       | Retired   | Not planned as a Zallet RPC; may become a Zallet CLI command.                                                                                  |
| [`dumpprivkey`](https://zcash.github.io/rpc/dumpprivkey.html)                             | —                                       | Retired   | Not planned by Zallet.                                                                                                                         |
| [`encryptwallet`](https://zcash.github.io/rpc/encryptwallet.html)                         | Zallet                                  | Replaced  | Key material is always encrypted from wallet setup; use `walletpassphrase`/`walletlock`.                                                       |
| [`getbalance`](https://zcash.github.io/rpc/getbalance.html)                               | Zallet                                  | Replaced  | Use `z_getbalanceforaccount` instead.                                                                                                          |
| [`getnewaddress`](https://zcash.github.io/rpc/getnewaddress.html)                         | Zallet                                  | Replaced  | Use `z_getnewaccount` + `z_getaddressforaccount`.                                                                                              |
| [`getrawchangeaddress`](https://zcash.github.io/rpc/getrawchangeaddress.html)             | —                                       | Retired   | Not needed: Zallet derives change addresses internally.                                                                                        |
| [`getreceivedbyaddress`](https://zcash.github.io/rpc/getreceivedbyaddress.html)           | [Zallet][zallet-status]                 | Planned   |                                                                                                                                                |
| [`gettransaction`](https://zcash.github.io/rpc/gettransaction.html)                       | Zallet                                  | Replaced  | Use `z_viewtransaction` instead.                                                                                                               |
| [`getunconfirmedbalance`](https://zcash.github.io/rpc/getunconfirmedbalance.html)         | [Zallet][zallet-status]                 | Planned   |                                                                                                                                                |
| [`getwalletinfo`](https://zcash.github.io/rpc/getwalletinfo.html)                         | [Zallet][zallet-getwalletinfo]          | Partial   | Balance fields are not populated. `unlocked_until`, `mnemonic_seedfp`, and `mnemonic_seedfps` are real; the remaining fields are placeholders. |
| [`importaddress`](https://zcash.github.io/rpc/importaddress.html)                         | Zallet                                  | Replaced  | Use the `zallet import-address` CLI command, or `z_importaddress`.                                                                             |
| [`importprivkey`](https://zcash.github.io/rpc/importprivkey.html)                         | [Zallet][zallet-status]                 | Planned   |                                                                                                                                                |
| [`importpubkey`](https://zcash.github.io/rpc/importpubkey.html)                           | Zallet                                  | Replaced  | Use `z_importaddress`.                                                                                                                         |
| [`importwallet`](https://zcash.github.io/rpc/importwallet.html)                           | Zallet                                  | Replaced  | Use `z_importkey` per key, or `zallet migrate-zcashd-wallet`.                                                                                  |
| [`keypoolrefill`](https://zcash.github.io/rpc/keypoolrefill.html)                         | —                                       | Retired   | Not needed: Zallet has no key pool.                                                                                                            |
| [`listaddresses`](https://zcash.github.io/rpc/listaddresses.html)                         | [Zallet][zallet-listaddresses]          | Done      |                                                                                                                                                |
| [`listaddressgroupings`](https://zcash.github.io/rpc/listaddressgroupings.html)           | —                                       | Retired   | Not planned by Zallet.                                                                                                                         |
| [`listlockunspent`](https://zcash.github.io/rpc/listlockunspent.html)                     | [Zallet][zallet-status]                 | Planned   |                                                                                                                                                |
| [`listreceivedbyaddress`](https://zcash.github.io/rpc/listreceivedbyaddress.html)         | [Zallet][zallet-status]                 | Planned   |                                                                                                                                                |
| [`listsinceblock`](https://zcash.github.io/rpc/listsinceblock.html)                       | [Zallet][zallet-status]                 | Planned   |                                                                                                                                                |
| [`listtransactions`](https://zcash.github.io/rpc/listtransactions.html)                   | [Zallet][zallet-status]                 | Planned   | Available today as the account-scoped `z_listtransactions`.                                                                                    |
| [`listunspent`](https://zcash.github.io/rpc/listunspent.html)                             | Zallet                                  | Replaced  | Use `z_listunspent` instead.                                                                                                                   |
| [`lockunspent`](https://zcash.github.io/rpc/lockunspent.html)                             | [Zallet][zallet-status]                 | Planned   |                                                                                                                                                |
| [`sendmany`](https://zcash.github.io/rpc/sendmany.html)                                   | Zallet                                  | Replaced  | Use `z_sendmany` or `z_sendfromaccount`.                                                                                                       |
| [`sendtoaddress`](https://zcash.github.io/rpc/sendtoaddress.html)                         | Zallet                                  | Replaced  | Use `z_sendfromaccount`; `z_sendmany` covers most uses too.                                                                                    |
| [`settxfee`](https://zcash.github.io/rpc/settxfee.html)                                   | —                                       | Retired   | Not needed: ZIP 317 fees are always used.                                                                                                      |
| [`signmessage`](https://zcash.github.io/rpc/signmessage.html)                             | [Zallet][zallet-signmessage]            | Done      | Shipped in 0.1.0-beta.3; Zallet's status page still lists it as not implemented.                                                               |
| [`walletconfirmbackup`](https://zcash.github.io/rpc/walletconfirmbackup.html)             | Zallet                                  | Replaced  | Internal to zcashd; use the `zallet confirm-backup` command.                                                                                   |
| `walletlock`                                                                              | [Zallet][zallet-walletlock]             | Done      | Unlocks and re-locks the key store.                                                                                                            |
| `walletpassphrase`                                                                        | [Zallet][zallet-walletpassphrase]       | Done      | Unlocks and re-locks the key store.                                                                                                            |
| `walletpassphrasechange`                                                                  | [Zallet][zallet-status]                 | Undecided |                                                                                                                                                |
| [`z_converttex`](https://zcash.github.io/rpc/z_converttex.html)                           | [Zallet][zallet-z_converttex]           | Done      |                                                                                                                                                |
| [`z_exportkey`](https://zcash.github.io/rpc/z_exportkey.html)                             | [Zallet][zallet-z_exportkey]            | Done      |                                                                                                                                                |
| [`z_exportviewingkey`](https://zcash.github.io/rpc/z_exportviewingkey.html)               | [Zallet][zallet-z_exportviewingkey]     | Done      | Shipped in 0.1.0-beta.2; Zallet's status page still lists it as not implemented.                                                               |
| [`z_exportwallet`](https://zcash.github.io/rpc/z_exportwallet.html)                       | [Zallet][zallet-status]                 | Planned   | Planned as a ZeWIF export, likely a CLI command.                                                                                               |
| [`z_getaddressforaccount`](https://zcash.github.io/rpc/z_getaddressforaccount.html)       | [Zallet][zallet-z_getaddressforaccount] | Done      |                                                                                                                                                |
| [`z_getbalance`](https://zcash.github.io/rpc/z_getbalance.html)                           | Zallet                                  | Replaced  | Use `z_getbalanceforaccount` instead.                                                                                                          |
| [`z_getbalanceforaccount`](https://zcash.github.io/rpc/z_getbalanceforaccount.html)       | [Zallet][zallet-z_getbalanceforaccount] | Done      |                                                                                                                                                |
| [`z_getbalanceforviewingkey`](https://zcash.github.io/rpc/z_getbalanceforviewingkey.html) | Zallet                                  | Replaced  | Imported viewing keys get accounts, so `z_getbalanceforaccount` covers them.                                                                   |
| [`z_getmigrationstatus`](https://zcash.github.io/rpc/z_getmigrationstatus.html)           | —                                       | Retired   | Sprout migration; Zallet does not support Sprout. May be revisited for a future pool migration.                                                |
| [`z_getnewaccount`](https://zcash.github.io/rpc/z_getnewaccount.html)                     | [Zallet][zallet-z_getnewaccount]        | Done      |                                                                                                                                                |
| [`z_getnewaddress`](https://zcash.github.io/rpc/z_getnewaddress.html)                     | Zallet                                  | Replaced  | Use `z_getnewaccount` + `z_getaddressforaccount`.                                                                                              |
| [`z_getnotescount`](https://zcash.github.io/rpc/z_getnotescount.html)                     | [Zallet][zallet-z_getnotescount]        | Done      |                                                                                                                                                |
| [`z_getoperationresult`](https://zcash.github.io/rpc/z_getoperationresult.html)           | [Zallet][zallet-z_getoperationresult]   | Done      |                                                                                                                                                |
| [`z_getoperationstatus`](https://zcash.github.io/rpc/z_getoperationstatus.html)           | [Zallet][zallet-z_getoperationstatus]   | Done      |                                                                                                                                                |
| [`z_gettotalbalance`](https://zcash.github.io/rpc/z_gettotalbalance.html)                 | [Zallet][zallet-z_gettotalbalance]      | Done      | Deprecated; `include_watchonly = false` is not honored yet. Prefer `z_getbalanceforaccount`.                                                   |
| [`z_importkey`](https://zcash.github.io/rpc/z_importkey.html)                             | [Zallet][zallet-z_importkey]            | Done      | Sapling extended spending keys only.                                                                                                           |
| [`z_importviewingkey`](https://zcash.github.io/rpc/z_importviewingkey.html)               | [Zallet][zallet-z_importviewingkey]     | Done      | Sapling extended full viewing keys only. Shipped in 0.1.0-beta.2; Zallet's status page still lists it as not implemented.                      |
| [`z_importwallet`](https://zcash.github.io/rpc/z_importwallet.html)                       | Zallet                                  | Replaced  | Use `z_importkey` per key, or `zallet migrate-zcashd-wallet`.                                                                                  |
| [`z_listaccounts`](https://zcash.github.io/rpc/z_listaccounts.html)                       | [Zallet][zallet-z_listaccounts]         | Done      |                                                                                                                                                |
| [`z_listaddresses`](https://zcash.github.io/rpc/z_listaddresses.html)                     | Zallet                                  | Replaced  | Use `listaddresses` instead.                                                                                                                   |
| [`z_listoperationids`](https://zcash.github.io/rpc/z_listoperationids.html)               | [Zallet][zallet-z_listoperationids]     | Done      |                                                                                                                                                |
| [`z_listreceivedbyaddress`](https://zcash.github.io/rpc/z_listreceivedbyaddress.html)     | [Zallet][zallet-status]                 | Planned   |                                                                                                                                                |
| [`z_listunifiedreceivers`](https://zcash.github.io/rpc/z_listunifiedreceivers.html)       | Zebra                                   | Done      | Zallet implements it too.                                                                                                                      |
| [`z_listunspent`](https://zcash.github.io/rpc/z_listunspent.html)                         | [Zallet][zallet-z_listunspent]          | Done      |                                                                                                                                                |
| [`z_mergetoaddress`](https://zcash.github.io/rpc/z_mergetoaddress.html)                   | [Zallet][zallet-status]†                | Planned   |                                                                                                                                                |
| [`z_sendmany`](https://zcash.github.io/rpc/z_sendmany.html)                               | [Zallet][zallet-z_sendmany]†            | Done      |                                                                                                                                                |
| [`z_setmigration`](https://zcash.github.io/rpc/z_setmigration.html)                       | —                                       | Retired   | Sprout migration; Zallet does not support Sprout. May be revisited for a future pool migration.                                                |
| [`z_shieldcoinbase`](https://zcash.github.io/rpc/z_shieldcoinbase.html)                   | [Zallet][zallet-z_shieldcoinbase]†      | Done      |                                                                                                                                                |
| [`z_viewtransaction`](https://zcash.github.io/rpc/z_viewtransaction.html)                 | [Zallet][zallet-z_viewtransaction]      | Done      |                                                                                                                                                |
| [`zcbenchmark`](https://zcash.github.io/rpc/zcbenchmark.html)                             | —                                       | Retired   | Out of scope for a validator node.                                                                                                             |
| [`zcsamplejoinsplit`](https://zcash.github.io/rpc/zcsamplejoinsplit.html)                 | —                                       | Retired   | Sprout-only.                                                                                                                                   |

### Disclosure

| Method                                                                                        | Owner | Status  | Notes                                            |
| --------------------------------------------------------------------------------------------- | ----- | ------- | ------------------------------------------------ |
| [`z_getpaymentdisclosure`](https://zcash.github.io/rpc/z_getpaymentdisclosure.html)           | —     | Retired | Sprout-only, like `z_validatepaymentdisclosure`. |
| [`z_validatepaymentdisclosure`](https://zcash.github.io/rpc/z_validatepaymentdisclosure.html) | —     | Retired | Sprout-only. [#8443]                             |

### Hidden

| Method                     | Owner                   | Status    | Notes                                                                                |
| -------------------------- | ----------------------- | --------- | ------------------------------------------------------------------------------------ |
| `dumpwallet`               | —                       | Retired   | Already removed from zcashd. Zallet plans a ZeWIF export (`z_exportwallet`) instead. |
| `invalidateblock`          | Zebra                   | Done      |                                                                                      |
| `reconsiderblock`          | Zebra                   | Partial   | Returns the reconsidered block hashes; zcashd returns null. [#11033]                 |
| `resendwallettransactions` | [Zallet][zallet-status] | Undecided |                                                                                      |
| `setmocktime`              | —                       | Retired   | Test-only; out of scope for a validator node.                                        |

## Zebra-only methods

API methods Zebra adds that `zcashd` never had. Listed so this page describes the
whole RPC boundary, not only a `zcashd` subset. None of them exist in `zcashd`
or the sidecar.

| Method                      | Owner | Status | Notes                                                                                                                                                                     |
| --------------------------- | ----- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `getbestblockheightandhash` | Zebra | Done   | Tip height and hash in one call, so a caller cannot read a torn pair across two requests.                                                                                 |
| `getstandardfee`            | Zebra | Done   | Returns `{standard_fee, version}`: the ZIP 317 marginal fee per logical action, in zatoshis, for the next block. The value depends on the next block's height ([#11557]). |
| `generatetoaddress`         | Zebra | Done   | Regtest only. Like `generate`, but pays the coinbase to a given address instead of the configured `mining.miner_address`, which lets one node fund several test wallets.  |
| `rpc.discover`              | Zebra | Done   | OpenRPC schema for every Zebra RPC method.                                                                                                                                |

## Notification transports

`zcashd` publishes chain and mempool events over ZMQ. Zebra does not implement
ZMQ today; publishing the `zcashd` topics is proposed in [#10881], [#11034], and
[#11036].

Zebra's equivalent is the **indexer gRPC service**
(`zebra-rpc/proto/indexer.proto`). It streams best-chain tip changes
(`ChainTipChange`), non-finalized blocks (`NonFinalizedStateChange`), and
mempool additions, invalidations, and mined transactions (`MempoolChange`), and
serves single blocks (`GetBlock`). Enable it by setting `indexer_listen_addr` in
the `[rpc]` section of `zebrad.toml`. It has no authentication, so bind it to a
loopback or private address. Zallet uses this service; see
[Running Zallet with Zebra](zallet.md).

| `zcashd` topic    | Owner | Status   | Notes                                                                                                                 |
| ----------------- | ----- | -------- | --------------------------------------------------------------------------------------------------------------------- |
| `zmqpubhashblock` | Zebra | Replaced | Indexer `ChainTipChange` stream (hash and height), or `block_notify_command`. ZMQ publishing is proposed in [#10881]. |
| `zmqpubrawblock`  | Zebra | Replaced | Indexer `NonFinalizedStateChange` stream. [#11034]                                                                    |
| `zmqpubhashtx`    | Zebra | Replaced | Indexer `MempoolChange` stream. [#11036]                                                                              |
| `zmqpubrawtx`     | Zebra | Replaced | Indexer `MempoolChange` stream plus `getrawtransaction`. [#11036]                                                     |

## Other integration points

| Integration point                   | Owner  | Status    | Notes                                                                                                                                                                    |
| ----------------------------------- | ------ | --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| REST interface (`-rest`, `/rest/*`) | —      | Undecided | JSON-RPC is the supported HTTP interface. [#11022]                                                                                                                       |
| `-blocknotify` hook                 | Zebra  | Done      | `block_notify_command` in the `[notify]` section of `zebrad.toml`; `%s` is replaced by the new tip's hash. Runs only once the node is close to the network tip. [#10726] |
| `-txexpirynotify` hook              | —      | Undecided | No hook. The indexer `MempoolChange` stream reports expired transactions as invalidated, without distinguishing expiry from other removals.                              |
| `-walletnotify` hook                | Zallet | Done      | `notify` in the `[external]` section of Zallet's config; `%s` is replaced by the transaction ID. `zallet migrate-zcash-conf` maps `walletnotify` to it.                  |
| `-alertnotify` hook                 | —      | Retired   | The Zcash alert system is retired; `zcashd` no longer sends alerts.                                                                                                      |

<!-- Zebra issues -->

[#8436]: https://github.com/ZcashFoundation/zebra/issues/8436
[#8440]: https://github.com/ZcashFoundation/zebra/issues/8440
[#8443]: https://github.com/ZcashFoundation/zebra/issues/8443
[#8645]: https://github.com/ZcashFoundation/zebra/issues/8645
[#8947]: https://github.com/ZcashFoundation/zebra/issues/8947
[#8952]: https://github.com/ZcashFoundation/zebra/issues/8952
[#9671]: https://github.com/ZcashFoundation/zebra/issues/9671
[#10329]: https://github.com/ZcashFoundation/zebra/issues/10329
[#10666]: https://github.com/ZcashFoundation/zebra/issues/10666
[#10726]: https://github.com/ZcashFoundation/zebra/pull/10726
[#10881]: https://github.com/ZcashFoundation/zebra/issues/10881
[#11022]: https://github.com/ZcashFoundation/zebra/issues/11022
[11022-decisions]: https://github.com/ZcashFoundation/zebra/issues/11022#issuecomment-6047333037
[#11028]: https://github.com/ZcashFoundation/zebra/issues/11028
[#11030]: https://github.com/ZcashFoundation/zebra/issues/11030
[#11033]: https://github.com/ZcashFoundation/zebra/issues/11033
[#11034]: https://github.com/ZcashFoundation/zebra/issues/11034
[#11036]: https://github.com/ZcashFoundation/zebra/issues/11036
[#11281]: https://github.com/ZcashFoundation/zebra/issues/11281
[#11557]: https://github.com/ZcashFoundation/zebra/pull/11557

<!-- Zallet book: per-method reference, generated from Zallet's RPC traits -->

[zallet-status]: https://zcash.github.io/zallet/zcashd/rpc_status.html
[zallet-decoderawtransaction]: https://zcash.github.io/zallet/rpc/index.html#decoderawtransaction
[zallet-decodescript]: https://zcash.github.io/zallet/rpc/index.html#decodescript
[zallet-getwalletinfo]: https://zcash.github.io/zallet/rpc/index.html#getwalletinfo
[zallet-help]: https://zcash.github.io/zallet/rpc/index.html#help
[zallet-listaddresses]: https://zcash.github.io/zallet/rpc/index.html#listaddresses
[zallet-signmessage]: https://zcash.github.io/zallet/rpc/index.html#signmessage
[zallet-verifymessage]: https://zcash.github.io/zallet/rpc/index.html#verifymessage
[zallet-walletlock]: https://zcash.github.io/zallet/rpc/index.html#walletlock
[zallet-walletpassphrase]: https://zcash.github.io/zallet/rpc/index.html#walletpassphrase
[zallet-z_converttex]: https://zcash.github.io/zallet/rpc/index.html#z_converttex
[zallet-z_exportkey]: https://zcash.github.io/zallet/rpc/index.html#z_exportkey
[zallet-z_exportviewingkey]: https://zcash.github.io/zallet/rpc/index.html#z_exportviewingkey
[zallet-z_getaddressforaccount]: https://zcash.github.io/zallet/rpc/index.html#z_getaddressforaccount
[zallet-z_getbalanceforaccount]: https://zcash.github.io/zallet/rpc/index.html#z_getbalanceforaccount
[zallet-z_getnewaccount]: https://zcash.github.io/zallet/rpc/index.html#z_getnewaccount
[zallet-z_getnotescount]: https://zcash.github.io/zallet/rpc/index.html#z_getnotescount
[zallet-z_getoperationresult]: https://zcash.github.io/zallet/rpc/index.html#z_getoperationresult
[zallet-z_getoperationstatus]: https://zcash.github.io/zallet/rpc/index.html#z_getoperationstatus
[zallet-z_gettotalbalance]: https://zcash.github.io/zallet/rpc/index.html#z_gettotalbalance
[zallet-z_importkey]: https://zcash.github.io/zallet/rpc/index.html#z_importkey
[zallet-z_importviewingkey]: https://zcash.github.io/zallet/rpc/index.html#z_importviewingkey
[zallet-z_listaccounts]: https://zcash.github.io/zallet/rpc/index.html#z_listaccounts
[zallet-z_listoperationids]: https://zcash.github.io/zallet/rpc/index.html#z_listoperationids
[zallet-z_listunspent]: https://zcash.github.io/zallet/rpc/index.html#z_listunspent
[zallet-z_sendmany]: https://zcash.github.io/zallet/rpc/index.html#z_sendmany
[zallet-z_shieldcoinbase]: https://zcash.github.io/zallet/rpc/index.html#z_shieldcoinbase
[zallet-z_viewtransaction]: https://zcash.github.io/zallet/rpc/index.html#z_viewtransaction
