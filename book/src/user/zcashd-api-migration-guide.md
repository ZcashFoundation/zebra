# zcashd API migration guide

Route node, chain, mempool, and mining calls to Zebra; wallet calls to Zallet.
Changing the RPC URL alone does not preserve parameters or responses.

The tables cover `zcashd` 6.20.0, Zebra 7.0.0, and
[Zallet 0.1.0-beta.3][zallet-rpc-source]. Zallet's reference follows development;
the release-specific restrictions here take precedence. Its direct `zebra`
backend is incompatible with Zebra 7.0.0; see [backend requirements](zallet.md)
for the unaffected RPC-based `zaino` backend.
For pool integrations, see the [payout RPC compatibility table and tested sidecar
procedure](mining-pools.md#wallet-rpc-compatibility); ownership does not imply that
an unmodified payout processor works with Zallet.

## Where the boundary falls today

| API requirement | Migration target |
| --- | --- |
| Chain, network, mempool, mining | Zebra, with the differences below |
| Wallet keys, balances, transaction construction | Zallet, with its [altered RPC semantics][zallet-semantics] |
| Existing wallet integration during migration | The restricted [zcashd-compat sidecar](zcashd-compat.md) |

## How to read the table

### Method

Method links point to the [`zcashd` RPC reference](https://zcash.github.io/rpc/).
The inventory includes hidden and wallet-encryption methods omitted there.

### Owner

| Owner | Meaning |
| --- | --- |
| **Zebra** | Implements the node-level method or the named replacement. |
| **Zallet** | Implements the wallet method or records its disposition in the [wallet method status][zallet-status]. |
| **—** | No implemented target RPC; Notes records proposals or non-RPC alternatives. |

Rows name Zebra when wallet state is unnecessary, even if Zallet also serves
the method (`getrawtransaction`, `validateaddress`, `z_listunifiedreceivers`,
and `z_converttex`).

The [sidecar](zcashd-compat.md) is temporary, not an API owner. Its network and
mempool methods describe its own state. Deprecation flags, index requirements,
sync gates, and wallet restrictions still apply:

| Marker | Sidecar behavior |
| --- | --- |
| _(none)_ | No special registration marker; other sidecar restrictions still apply. |
| **†** | Wallet transaction construction is subject to the [shielded-pool restrictions](zcashd-compat.md#wallet-shielded-pool-support-orchard--ironwood). |
| **\*** | The sidecar does not register the method; calls return `-32601`. |

### Status

Status describes the named target, not the temporary sidecar.

| Status | Meaning |
| --- | --- |
| **Done** | Implemented; responses need not be byte-identical. |
| **Partial** | Implemented with restrictions or semantic differences in Notes. |
| **Stub** | Returns success without performing the operation. |
| **Replaced** | An available replacement requires client changes. |
| **Planned** | Recorded implementation decision; not implemented. |
| **Deferred** | Waits on the named work. |
| **Unavailable** | Not implemented; no delivery commitment. |
| **Undecided** | Disposition remains open. |
| **Retired** | Not part of the target API. |

### Decisions and open questions

Node-method dispositions follow [#11022][11022-decisions]. Issue [#11033]
tracks remaining shared-RPC differences.

## RPC methods

### Control

| Method                                                                                | Owner                 | Status   | Notes                                                                                                |
| ------------------------------------------------------------------------------------- | --------------------- | -------- | ---------------------------------------------------------------------------------------------------- |
| [`getexperimentalfeatures`](https://zcash.github.io/rpc/getexperimentalfeatures.html) | — | Retired | |
| [`getinfo`](https://zcash.github.io/rpc/getinfo.html) | Zebra | Partial | No `timeoffset` or wallet balances. The unset `proxy` field is omitted; `paytxfee` is always `0`. |
| [`getmemoryinfo`](https://zcash.github.io/rpc/getmemoryinfo.html) | — | Retired | |
| [`help`](https://zcash.github.io/rpc/help.html)                                       | [Zallet][zallet-help] | Done     | Zallet serves `help` for its own RPC; Zebra does not.                                                |
| [`setlogfilter`](https://zcash.github.io/rpc/setlogfilter.html) | — | Replaced | Use the tracing [`/filter` endpoint](tracing.md#dynamic-tracing); requires the `filter-reload` build feature. |
| [`stop`](https://zcash.github.io/rpc/stop.html) | Zebra | Partial | Regtest on non-Windows targets only. Returns `Zebra server stopping`, not `Zcash server stopping`. [#11033] |

### Blockchain

| Method                                                                          | Owner | Status  | Notes                                                                                                                                                                                                                                                                                                                                      |
| ------------------------------------------------------------------------------- | ----- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [`getbestblockhash`](https://zcash.github.io/rpc/getbestblockhash.html)         | Zebra | Done    |                                                                                                                                                                                                                                                                                                                                            |
| [`getblock`](https://zcash.github.io/rpc/getblock.html) | Zebra | Partial | No `authdataroot`, `chainhistoryroot`, `chainwork`, or `anchor`. The `verbosity` argument is numeric, not boolean. Adds `nTx` and Ironwood tree data. Verbosity `3` adds transparent-input `prevout` data and non-coinbase `fee` where spent outputs are available. |
| [`getblockchaininfo`](https://zcash.github.io/rpc/getblockchaininfo.html) | Zebra | Partial | No `initial_block_download_complete`, `softforks`, or `pruneheight`. The `chainwork` value is numeric `0`, not a hex string; `commitments` is `0`, `pruned` is `false`, and `headers` equals the committed tip. [#11033] |
| [`getblockcount`](https://zcash.github.io/rpc/getblockcount.html)               | Zebra | Done    |                                                                                                                                                                                                                                                                                                                                            |
| [`getblockdeltas`](https://zcash.github.io/rpc/getblockdeltas.html) | — | Retired | Build explorer indexes on [indexer gRPC](#notification-transports). |
| [`getblockhash`](https://zcash.github.io/rpc/getblockhash.html)                 | Zebra | Done    |                                                                                                                                                                                                                                                                                                                                            |
| [`getblockhashes`](https://zcash.github.io/rpc/getblockhashes.html) | — | Retired | Build explorer indexes on [indexer gRPC](#notification-transports). [#8436] |
| [`getblockheader`](https://zcash.github.io/rpc/getblockheader.html) | Zebra | Partial | Omits `zcashd`'s undocumented `chainwork`; adds `blockcommitments`. |
| [`getchaintips`](https://zcash.github.io/rpc/getchaintips.html) | — | Planned | Tracked in [#10666]. |
| [`getdifficulty`](https://zcash.github.io/rpc/getdifficulty.html) | Zebra | Partial | Computes the ratio from the high 128 bits of expanded targets. Returns an error when state cannot supply chain information, where `zcashd` can return `1.0` for an empty chain. |
| [`getmempoolinfo`](https://zcash.github.io/rpc/getmempoolinfo.html)             | Zebra | Partial | The regtest-only key is spelled `fully_notified`; zcashd spells it `fullyNotified`.                                                                                                                                                                                                                                                        |
| [`getrawmempool`](https://zcash.github.io/rpc/getrawmempool.html) | Zebra | Partial | In verbose output, `modifiedfee` equals `fee`; Zebra has no `prioritisetransaction` fee deltas. |
| [`getspentinfo`](https://zcash.github.io/rpc/getspentinfo.html) | — | Planned | Intended to use Zebra's `indexer` spending-transaction index. |
| [`gettxout`](https://zcash.github.io/rpc/gettxout.html) | Zebra | Partial | P2PK outputs do not receive the synthetic P2PKH address reported by `zcashd`; the script address list is empty. |
| [`gettxoutproof`](https://zcash.github.io/rpc/gettxoutproof.html) | — | Planned | [Decision][11022-decisions]. |
| [`gettxoutsetinfo`](https://zcash.github.io/rpc/gettxoutsetinfo.html) | — | Retired | Build explorer indexes on [indexer gRPC](#notification-transports). |
| [`verifychain`](https://zcash.github.io/rpc/verifychain.html) | — | Retired | |
| [`verifytxoutproof`](https://zcash.github.io/rpc/verifytxoutproof.html) | — | Planned | [Decision][11022-decisions]. |
| [`z_getsubtreesbyindex`](https://zcash.github.io/rpc/z_getsubtreesbyindex.html) | Zebra | Done | Accepts `sapling`, `orchard`, and `ironwood`. During subtree-index rebuilding, a not-yet-rebuilt range can return an empty list. |
| [`z_gettreestate`](https://zcash.github.io/rpc/z_gettreestate.html) | Zebra | Partial | Returns `finalRoot` and `finalState` for available Sapling, Orchard, and Ironwood trees. Omits Sprout entirely and does not return `skipHash`. Ironwood appears from NU6.3. [#11033] |

### Address index

| Method                                                                    | Owner | Status  | Notes                                                                                                       |
| ------------------------------------------------------------------------- | ----- | ------- | ----------------------------------------------------------------------------------------------------------- |
| [`getaddressbalance`](https://zcash.github.io/rpc/getaddressbalance.html) | Zebra | Done | Accepts a single transparent address or an object containing `addresses`; no separate address-index flag is required. |
| [`getaddressdeltas`](https://zcash.github.io/rpc/getaddressdeltas.html) | — | Retired | Build explorer indexes on [indexer gRPC](#notification-transports). [#8440] |
| [`getaddressmempool`](https://zcash.github.io/rpc/getaddressmempool.html) | — | Retired | Build explorer indexes on [indexer gRPC](#notification-transports). |
| [`getaddresstxids`](https://zcash.github.io/rpc/getaddresstxids.html) | Zebra | Partial | Accepts a single address or an `addresses` object with optional inclusive `start`/`end` heights. Limits requests to 10,000 addresses and 100,000 matching transactions; narrow oversized ranges. |
| [`getaddressutxos`](https://zcash.github.io/rpc/getaddressutxos.html) | Zebra | Partial | Supports `chainInfo` and optional `startHeight`/`maxEntries` result limits. Limits requests to 10,000 addresses and results to 100,000 outputs. |

### Mining

| Method                                                                            | Owner   | Status  | Notes                                                                                                                                                                                                    |
| --------------------------------------------------------------------------------- | ------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`getblocksubsidy`](https://zcash.github.io/rpc/getblocksubsidy.html) | Zebra | Partial | Returns historical subsidies, including founders' rewards before the first halving. After reserve-dependent reissuance begins, the preceding block must be available; arbitrary future subsidies cannot be forecast. |
| [`getblocktemplate`](https://zcash.github.io/rpc/getblocktemplate.html) | Zebra\* | Done | Implements long polling and proposal mode. Adds `maxtime` and `submitold`; advertises `capabilities: ["proposal"]` and `mutable: ["time"]`. |
| [`getlocalsolps`](https://zcash.github.io/rpc/getlocalsolps.html) | — | Retired | |
| [`getmininginfo`](https://zcash.github.io/rpc/getmininginfo.html)                 | Zebra   | Partial | No `errors`, `errorstimestamp`, `genproclimit`, `localsolps`, `pooledtx`, or `generate`.                                                                                                   |
| [`getnetworkhashps`](https://zcash.github.io/rpc/getnetworkhashps.html)           | Zebra   | Done    | Deprecated alias for `getnetworksolps`, in both zcashd and Zebra.                                                                                                                                        |
| [`getnetworksolps`](https://zcash.github.io/rpc/getnetworksolps.html)             | Zebra   | Done    |                                                                                                                                                                                                          |
| [`prioritisetransaction`](https://zcash.github.io/rpc/prioritisetransaction.html) | — | Planned | [Decision][11022-decisions]. |
| [`submitblock`](https://zcash.github.io/rpc/submitblock.html)                     | Zebra\* | Partial | Returns only `null`, `duplicate`, or `rejected`. zcashd also returns `duplicate-invalid`, `duplicate-inconclusive`, `inconclusive`, and the specific BIP-22 reject reason in place of a bare `rejected`. |

### Generating

| Method                                                        | Owner   | Status  | Notes                                                                                 |
| ------------------------------------------------------------- | ------- | ------- | ------------------------------------------------------------------------------------- |
| [`generate`](https://zcash.github.io/rpc/generate.html) | Zebra\* | Partial | Requires a network with proof-of-work validation disabled, including Regtest. Pays the configured `[mining] miner_address`. |
| [`getgenerate`](https://zcash.github.io/rpc/getgenerate.html) | —\*     | Retired | Controls zcashd's built-in CPU miner. Mine with `getblocktemplate` and `submitblock`. |
| [`setgenerate`](https://zcash.github.io/rpc/setgenerate.html) | —\*     | Retired | Controls zcashd's built-in CPU miner. Mine with `getblocktemplate` and `submitblock`. |

### Network

| Method                                                                      | Owner   | Status   | Notes                                                                                                                                                           |
| --------------------------------------------------------------------------- | ------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`addnode`](https://zcash.github.io/rpc/addnode.html)                       | Zebra\* | Partial  | Regtest only, and only the `add` command. [#10329]                                                                                                              |
| [`clearbanned`](https://zcash.github.io/rpc/clearbanned.html) | — | Deferred | Waits on v2 P2P ([#11281]). |
| [`disconnectnode`](https://zcash.github.io/rpc/disconnectnode.html) | — | Deferred | Waits on v2 P2P ([#11281]). |
| [`getaddednodeinfo`](https://zcash.github.io/rpc/getaddednodeinfo.html) | — | Deferred | Waits on v2 P2P ([#11281]). |
| [`getconnectioncount`](https://zcash.github.io/rpc/getconnectioncount.html) | Zebra   | Replaced | Use `connections` from `getnetworkinfo` or `getinfo`.                                                                                                           |
| [`getdeprecationinfo`](https://zcash.github.io/rpc/getdeprecationinfo.html) | Zebra | Partial | Returns `end_of_service` on Mainnet only. Omits `version`, `subversion`, `deprecated_features`, and `disabled_features`. |
| [`getnettotals`](https://zcash.github.io/rpc/getnettotals.html) | — | Deferred | Waits on v2 P2P ([#11281]). |
| [`getnetworkinfo`](https://zcash.github.io/rpc/getnetworkinfo.html)         | Zebra   | Partial  | No `warningstimestamp`. `localaddresses` and `warnings` are empty. [#11030]                                                                                     |
| [`getpeerinfo`](https://zcash.github.io/rpc/getpeerinfo.html) | Zebra | Partial | Returns `addr`, `services`, `lastrecv`, `inbound`, `banscore`, `subver`, `version`, `startingheight`, and the added `connection_state`; `pingtime` and `pingwait` are optional. Other `zcashd` per-peer fields are absent. |
| [`listbanned`](https://zcash.github.io/rpc/listbanned.html) | — | Deferred | Waits on v2 P2P ([#11281]). |
| [`ping`](https://zcash.github.io/rpc/ping.html) | Zebra | Stub | Does not request a ping; peers use automatic heartbeats ([#11028]). |
| [`setban`](https://zcash.github.io/rpc/setban.html) | — | Deferred | Waits on v2 P2P ([#11281]). |

### Raw transactions

| Method                                                                          | Owner                                 | Status   | Notes                                                                                                                                                                                |
| ------------------------------------------------------------------------------- | ------------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [`createrawtransaction`](https://zcash.github.io/rpc/createrawtransaction.html) | Zallet | Replaced | Use Zallet's partially created Zcash transaction (PCZT) methods: `pczt_create`, `pczt_prove`, `pczt_sign`, and `pczt_extract`. [#8952] |
| [`decoderawtransaction`](https://zcash.github.io/rpc/decoderawtransaction.html) | [Zallet][zallet-decoderawtransaction] | Done | [#8645] |
| [`decodescript`](https://zcash.github.io/rpc/decodescript.html)                 | [Zallet][zallet-decodescript]         | Done     |                                                                                                                                                                                      |
| [`fundrawtransaction`](https://zcash.github.io/rpc/fundrawtransaction.html)     | Zallet                                | Replaced | Use Zallet's PCZT methods instead.                                                                                                                                                   |
| [`getrawtransaction`](https://zcash.github.io/rpc/getrawtransaction.html) | Zebra | Partial | Uses numeric verbosity. Outputs expose `valueZat`, not `valueSat`, and omit `spentTxId`, `spentIndex`, and `spentHeight`. P2PK outputs have no synthetic address ([#9671]). Supports Ironwood transaction data. |
| [`sendrawtransaction`](https://zcash.github.io/rpc/sendrawtransaction.html)     | Zebra                                 | Partial  | `allowhighfees` is accepted and ignored.                                                                                                                                             |
| [`signrawtransaction`](https://zcash.github.io/rpc/signrawtransaction.html)     | Zallet                                | Replaced | Use Zallet's PCZT methods instead.                                                                                                                                                   |

### Util

| Method                                                                    | Owner                          | Status  | Notes                                                                                                                                                                                              |
| ------------------------------------------------------------------------- | ------------------------------ | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`createmultisig`](https://zcash.github.io/rpc/createmultisig.html) | [Zallet][zallet-status] | Unavailable | |
| [`validateaddress`](https://zcash.github.io/rpc/validateaddress.html)     | Zebra                          | Partial | No `scriptPubKey`. Adds `isscript`.                                                                                                                                                                |
| [`verifymessage`](https://zcash.github.io/rpc/verifymessage.html) | [Zallet][zallet-verifymessage] | Done | [#8947] |
| [`z_validateaddress`](https://zcash.github.io/rpc/z_validateaddress.html) | Zebra | Partial | Emits `address_type`, not the deprecated `type` alias. Omits Sapling key details, rejects Sprout addresses, and reports `ismine: false`. |

### Wallet

Build Zallet with [`zcashd-import`](https://github.com/zcash/zallet/blob/v0.1.0-beta.3/zallet-core/Cargo.toml#L201-L218)
for its wallet-migration CLI. This also enables `transparent-key-import`;
without it, the registered `z_importaddress` RPC returns an error.

| Method                                                                                    | Owner                                   | Status    | Notes                                                                                                                                          |
| ----------------------------------------------------------------------------------------- | --------------------------------------- | --------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| [`addmultisigaddress`](https://zcash.github.io/rpc/addmultisigaddress.html) | [Zallet][zallet-status] | Unavailable | |
| [`backupwallet`](https://zcash.github.io/rpc/backupwallet.html) | — | Retired | No Zallet RPC equivalent; `zallet backup` is unreleased, not in beta.3. |
| [`dumpprivkey`](https://zcash.github.io/rpc/dumpprivkey.html)                             | —                                       | Retired   | Not planned by Zallet.                                                                                                                         |
| [`encryptwallet`](https://zcash.github.io/rpc/encryptwallet.html)                         | Zallet                                  | Replaced  | Key material is always encrypted from wallet setup; use `walletpassphrase`/`walletlock`.                                                       |
| [`getbalance`](https://zcash.github.io/rpc/getbalance.html)                               | Zallet                                  | Replaced  | Use `z_getbalanceforaccount` instead.                                                                                                          |
| [`getnewaddress`](https://zcash.github.io/rpc/getnewaddress.html)                         | Zallet                                  | Replaced  | Use `z_getnewaccount` + `z_getaddressforaccount`.                                                                                              |
| [`getrawchangeaddress`](https://zcash.github.io/rpc/getrawchangeaddress.html)             | —                                       | Retired   | Not needed: Zallet derives change addresses internally.                                                                                        |
| [`getreceivedbyaddress`](https://zcash.github.io/rpc/getreceivedbyaddress.html) | [Zallet][zallet-status] | Unavailable | |
| [`gettransaction`](https://zcash.github.io/rpc/gettransaction.html)                       | Zallet                                  | Replaced  | Use `z_viewtransaction` instead.                                                                                                               |
| [`getunconfirmedbalance`](https://zcash.github.io/rpc/getunconfirmedbalance.html) | [Zallet][zallet-status] | Unavailable | |
| [`getwalletinfo`](https://zcash.github.io/rpc/getwalletinfo.html) | [Zallet][zallet-getwalletinfo] | Partial | Balances and counters are placeholders; `mnemonic_seedfp` is `"TODO"` and `mnemonic_seedfps` is absent. Only `unlocked_until` is live. Use dedicated balance methods. |
| [`importaddress`](https://zcash.github.io/rpc/importaddress.html) | Zallet | Replaced | Use `zallet import-address` for a bare address, public key, or redeem script. The `z_importaddress` RPC covers public keys and redeem scripts, not bare addresses. |
| [`importprivkey`](https://zcash.github.io/rpc/importprivkey.html) | [Zallet][zallet-status] | Unavailable | |
| [`importpubkey`](https://zcash.github.io/rpc/importpubkey.html)                           | Zallet                                  | Replaced  | Use `z_importaddress`.                                                                                                                         |
| [`importwallet`](https://zcash.github.io/rpc/importwallet.html)                           | Zallet                                  | Replaced  | Use `z_importkey` per key, or `zallet migrate-zcashd-wallet`.                                                                                  |
| [`keypoolrefill`](https://zcash.github.io/rpc/keypoolrefill.html)                         | —                                       | Retired   | Not needed: Zallet has no key pool.                                                                                                            |
| [`listaddresses`](https://zcash.github.io/rpc/listaddresses.html) | [Zallet][zallet-listaddresses] | Partial | Adds `derived_transparent`, expands watch-only coverage, and uses bech32m seed fingerprints. See [altered semantics][zallet-semantics]. |
| [`listaddressgroupings`](https://zcash.github.io/rpc/listaddressgroupings.html)           | —                                       | Retired   | Not planned by Zallet.                                                                                                                         |
| [`listlockunspent`](https://zcash.github.io/rpc/listlockunspent.html) | [Zallet][zallet-status] | Unavailable | |
| [`listreceivedbyaddress`](https://zcash.github.io/rpc/listreceivedbyaddress.html) | [Zallet][zallet-status] | Unavailable | |
| [`listsinceblock`](https://zcash.github.io/rpc/listsinceblock.html) | [Zallet][zallet-status] | Unavailable | |
| [`listtransactions`](https://zcash.github.io/rpc/listtransactions.html) | [Zallet][zallet-status] | Unavailable | Use `z_listtransactions`, optionally filtered by account. |
| [`listunspent`](https://zcash.github.io/rpc/listunspent.html)                             | Zallet                                  | Replaced  | Use `z_listunspent` instead.                                                                                                                   |
| [`lockunspent`](https://zcash.github.io/rpc/lockunspent.html) | [Zallet][zallet-status] | Unavailable | |
| [`sendmany`](https://zcash.github.io/rpc/sendmany.html)                                   | Zallet                                  | Replaced  | Use `z_sendmany` or `z_sendfromaccount`.                                                                                                       |
| [`sendtoaddress`](https://zcash.github.io/rpc/sendtoaddress.html) | Zallet | Replaced | Use `z_sendfromaccount`; see `z_sendmany` for its restrictions. |
| [`settxfee`](https://zcash.github.io/rpc/settxfee.html) | — | Retired | Zallet uses [ZIP 317](https://zips.z.cash/zip-0317) fees rather than a wallet-wide fixed fee. |
| [`signmessage`](https://zcash.github.io/rpc/signmessage.html) | [Zallet][zallet-signmessage] | Done | Signs with an unlocked wallet's transparent P2PKH key; ephemeral addresses are refused. |
| [`walletconfirmbackup`](https://zcash.github.io/rpc/walletconfirmbackup.html)             | Zallet                                  | Replaced  | Internal to zcashd; use the `zallet confirm-backup` command.                                                                                   |
| `walletlock` | [Zallet][zallet-walletlock] | Done | |
| `walletpassphrase` | [Zallet][zallet-walletpassphrase] | Done | |
| `walletpassphrasechange`                                                                  | [Zallet][zallet-status]                 | Undecided |                                                                                                                                                |
| [`z_converttex`](https://zcash.github.io/rpc/z_converttex.html) | Zebra | Partial | Converts P2PKH to [ZIP 320](https://zips.z.cash/zip-0320) TEX. Returns `-5` for Sprout or unsupported unified receivers where `zcashd` returns `-8`. |
| [`z_exportkey`](https://zcash.github.io/rpc/z_exportkey.html) | [Zallet][zallet-z_exportkey] | Partial | Sapling extended spending keys only. |
| [`z_exportviewingkey`](https://zcash.github.io/rpc/z_exportviewingkey.html) | [Zallet][zallet-z_exportviewingkey] | Partial | Supports Sapling and unified viewing keys, adds `ivk`, and rejects Sprout. See [altered semantics][zallet-semantics]. |
| [`z_exportwallet`](https://zcash.github.io/rpc/z_exportwallet.html) | [Zallet][zallet-status] | Unavailable | Zallet plans a ZeWIF export workflow. |
| [`z_getaddressforaccount`](https://zcash.github.io/rpc/z_getaddressforaccount.html) | [Zallet][zallet-z_getaddressforaccount] | Partial | Accepts account UUIDs; derives time-based addresses when neither a transparent receiver nor an explicit index is requested. See [response changes][zallet-semantics]. |
| [`z_getbalance`](https://zcash.github.io/rpc/z_getbalance.html)                           | Zallet                                  | Replaced  | Use `z_getbalanceforaccount` instead.                                                                                                          |
| [`z_getbalanceforaccount`](https://zcash.github.io/rpc/z_getbalanceforaccount.html)       | [Zallet][zallet-z_getbalanceforaccount] | Done      |                                                                                                                                                |
| [`z_getbalanceforviewingkey`](https://zcash.github.io/rpc/z_getbalanceforviewingkey.html) | Zallet                                  | Replaced  | Imported viewing keys get accounts, so `z_getbalanceforaccount` covers them.                                                                   |
| [`z_getmigrationstatus`](https://zcash.github.io/rpc/z_getmigrationstatus.html) | — | Retired | Zallet does not support Sprout migration. |
| [`z_getnewaccount`](https://zcash.github.io/rpc/z_getnewaccount.html) | [Zallet][zallet-z_getnewaccount] | Partial | Requires `account_name`; `seedfp` is required when the wallet contains multiple seeds. |
| [`z_getnewaddress`](https://zcash.github.io/rpc/z_getnewaddress.html)                     | Zallet                                  | Replaced  | Use `z_getnewaccount` + `z_getaddressforaccount`.                                                                                              |
| [`z_getnotescount`](https://zcash.github.io/rpc/z_getnotescount.html)                     | [Zallet][zallet-z_getnotescount]        | Done      |                                                                                                                                                |
| [`z_getoperationresult`](https://zcash.github.io/rpc/z_getoperationresult.html)           | [Zallet][zallet-z_getoperationresult]   | Done      |                                                                                                                                                |
| [`z_getoperationstatus`](https://zcash.github.io/rpc/z_getoperationstatus.html)           | [Zallet][zallet-z_getoperationstatus]   | Done      |                                                                                                                                                |
| [`z_gettotalbalance`](https://zcash.github.io/rpc/z_gettotalbalance.html) | [Zallet][zallet-z_gettotalbalance] | Partial | Deprecated; `include_watchonly = false` is not honored. Prefer `z_getbalanceforaccount`. |
| [`z_importkey`](https://zcash.github.io/rpc/z_importkey.html) | [Zallet][zallet-z_importkey] | Partial | Sapling extended spending keys only. |
| [`z_importviewingkey`](https://zcash.github.io/rpc/z_importviewingkey.html) | [Zallet][zallet-z_importviewingkey] | Partial | Sapling extended full viewing keys only. |
| [`z_importwallet`](https://zcash.github.io/rpc/z_importwallet.html)                       | Zallet                                  | Replaced  | Use `z_importkey` per key, or `zallet migrate-zcashd-wallet`.                                                                                  |
| [`z_listaccounts`](https://zcash.github.io/rpc/z_listaccounts.html) | [Zallet][zallet-z_listaccounts] | Partial | Adds UUIDs, names, seed fingerprints, and `include_addresses`. Omits `account` and `diversifier_index` when derivation is unknown. See [response changes][zallet-semantics]. |
| [`z_listaddresses`](https://zcash.github.io/rpc/z_listaddresses.html)                     | Zallet                                  | Replaced  | Use `listaddresses` instead.                                                                                                                   |
| [`z_listoperationids`](https://zcash.github.io/rpc/z_listoperationids.html)               | [Zallet][zallet-z_listoperationids]     | Done      |                                                                                                                                                |
| [`z_listreceivedbyaddress`](https://zcash.github.io/rpc/z_listreceivedbyaddress.html) | [Zallet][zallet-status] | Unavailable | |
| [`z_listunifiedreceivers`](https://zcash.github.io/rpc/z_listunifiedreceivers.html) | Zebra | Done | |
| [`z_listunspent`](https://zcash.github.io/rpc/z_listunspent.html) | [Zallet][zallet-z_listunspent] | Partial | Includes transparent outputs; uses `value`, `account_uuid`, and `is_watch_only` instead of `amount`, `account`, and `spendable`. Ignores `include_watchonly`. See [response changes][zallet-semantics]. |
| [`z_mergetoaddress`](https://zcash.github.io/rpc/z_mergetoaddress.html) | [Zallet][zallet-status]† | Unavailable | |
| [`z_sendmany`](https://zcash.github.io/rpc/z_sendmany.html) | [Zallet][zallet-z_sendmany]† | Partial | Requires a null or omitted fee; defaults to the [ZIP 315](https://zips.z.cash/zip-0315) confirmation policy. Operation results add `txids` and `broadcast`. See [altered semantics][zallet-semantics]. |
| [`z_setmigration`](https://zcash.github.io/rpc/z_setmigration.html) | — | Retired | Zallet does not support Sprout migration. |
| [`z_shieldcoinbase`](https://zcash.github.io/rpc/z_shieldcoinbase.html) | [Zallet][zallet-z_shieldcoinbase]† | Partial | Accepts one address or account UUID, not `*`; rejects non-null fees. Allows only `AllowRevealedSenders` or `AllowLinkingAccountAddresses`. Check the added `broadcast` result before assuming submission. |
| [`z_viewtransaction`](https://zcash.github.io/rpc/z_viewtransaction.html) | [Zallet][zallet-z_viewtransaction] | Partial | Includes transparent activity, account effects, and additional top-level fields. See [altered semantics][zallet-semantics]. |
| [`zcbenchmark`](https://zcash.github.io/rpc/zcbenchmark.html) | — | Retired | |
| [`zcsamplejoinsplit`](https://zcash.github.io/rpc/zcsamplejoinsplit.html)                 | —                                       | Retired   | Sprout-only.                                                                                                                                   |

### Disclosure

| Method                                                                                        | Owner | Status  | Notes                                            |
| --------------------------------------------------------------------------------------------- | ----- | ------- | ------------------------------------------------ |
| [`z_getpaymentdisclosure`](https://zcash.github.io/rpc/z_getpaymentdisclosure.html) | — | Retired | Sprout-only. |
| [`z_validatepaymentdisclosure`](https://zcash.github.io/rpc/z_validatepaymentdisclosure.html) | —     | Retired | Sprout-only. [#8443]                             |

### Hidden

| Method                     | Owner                   | Status    | Notes                                                                                |
| -------------------------- | ----------------------- | --------- | ------------------------------------------------------------------------------------ |
| `dumpwallet` | — | Retired | The legacy handler returns `Method not found` and directs callers to `z_exportwallet`. Zallet records a planned ZeWIF export instead. |
| `invalidateblock` | Zebra | Partial | Only removes blocks present in non-finalized state; it is not a general finalized-chain rollback mechanism. |
| `reconsiderblock` | Zebra | Partial | Uses the cache of previously invalidated blocks and returns reconsidered hashes rather than `null`. [#11033] |
| `resendwallettransactions` | [Zallet][zallet-status] | Undecided |                                                                                      |
| `setmocktime` | — | Retired | Test-only. |

## Zebra-only methods

| Method                      | Owner | Status | Notes                                                                                                                                                                     |
| --------------------------- | ----- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `getbestblockheightandhash` | Zebra | Done   | Tip height and hash in one call, so a caller cannot read a torn pair across two requests.                                                                                 |
| `getstandardfee` | Zebra | Done | Returns `{standard_fee, version}`: the next block's marginal fee per logical action, in zatoshis. Version is `0`; the fee is height-dependent ([#11557]). |
| `generatetoaddress` | Zebra | Done | Requires proof-of-work validation to be disabled, including Regtest. Pays the supplied address instead of `[mining] miner_address`. |
| `preciousblock` | Zebra | Done | Returns null; see the tip-selection rules below. |
| `rpc.discover`              | Zebra | Done   | OpenRPC schema for every Zebra RPC method.                                                                                                                                |

`preciousblock` prefers a non-finalized chain tip only over competing tips with
equal cumulative work; higher-work chains always take precedence. The last call
for competing equal-work tips wins. Preferences are not persisted and are lost
on restart. Finalized blocks and known non-tip blocks succeed without changing
the preference; an unknown hash returns `Block not found`.

## Notification transports

Zebra publishes zcashd-compatible ZMQ `hashblock` notifications when built with
the optional [`zmq` feature](install.md#cargo-features) and configured with
[`[notify] zmqpubhashblock`](config.md#notify), for example
`"tcp://127.0.0.1:28332"`. The publisher is off by default and has no
authentication; expose it only to trusted clients. Like block notify, it is
suppressed during initial sync. See
[ZMQ block notifications](mining.md#zmq-block-notifications) for framing and
best-effort delivery semantics. The other ZMQ topics remain proposed in
[#11034] and [#11036]. Zebra's [indexer gRPC streams][indexer-proto] cover
them but require client changes:

Set [`[rpc] indexer_listen_addr`](config.md#rpc); the streams need no `indexer`
build feature. Keep the unauthenticated, plaintext listener on loopback or a
protected private network; JSON-RPC cookie authentication does not apply.

| `zcashd` topic | Owner | Status | Available alternative and limitation |
| --- | --- | --- | --- |
| `zmqpubhashblock` | Zebra | Done | Optional `zmq` feature and `[notify] zmqpubhashblock` ([#10881]). Indexer `ChainTipChange`, block-notify, and mining long polling are alternatives. |
| `zmqpubrawblock` | Zebra | Replaced | Indexer `NonFinalizedStateChange` streams serialized non-finalized blocks, including forks; use `GetBlock` for a best-chain block by hash or height. This is not identical to a best-block ZMQ topic ([#11034]). |
| `zmqpubhashtx` | Zebra | Replaced | Indexer `MempoolChange` reports added, invalidated, and mined transaction IDs. Its events are not the complete `zcashd` ZMQ transaction contract ([#11036]). |
| `zmqpubrawtx` | Zebra | Replaced | Combine `MempoolChange` with `getrawtransaction`; a transaction removed before the lookup can be unavailable. The stream does not contain transaction bytes ([#11036]). |

Streams can close when a consumer falls behind. Reconcile state through RPC
after reconnecting rather than treating notifications as a durable event log.
The non-finalized stream accepts known chain-tip hashes; an empty request
streams the current non-finalized state before subsequent updates.

## Other integration points

| Integration point | Owner | Status | Notes |
| --- | --- | --- | --- |
| REST interface (`-rest`, `/rest/*`) | — | Undecided | Use JSON-RPC; REST disposition remains open in [#11022]. |
| `-blocknotify` hook | Zebra | Done | Set [`[notify] block_notify_command`](config.md#notify); notifications run only near the tip and can coalesce intermediate changes. [#10726] |
| `-txexpirynotify` hook | — | Undecided | Indexer `MempoolChange` reports invalidation without an expiry reason. [#11022] |
| `-walletnotify` hook | Zallet | Unavailable | Zallet beta.3 accepts `[external] notify`, and `migrate-zcash-conf` maps `walletnotify` to it, but startup warns that the hook is not implemented. See its [release notes][zallet-changelog]. |
| `-alertnotify` hook | — | Retired | No Zebra alert-system hook. |

## Configuration mappings

Zebra reads TOML, not `zcash.conf`. These are behavioral mappings, not accepted
aliases. See the [configuration reference](config.md) for defaults and
environment-variable overrides.

| `zcashd` setting | Zebra setting or migration action |
| --- | --- |
| `conf` | Select Zebra's TOML with `zebrad -c zebrad.toml start`. |
| `datadir` | Set `[state] cache_dir` for Zebra's own database. This does not import a `zcashd` directory. |
| `testnet`, `regtest` | Set `[network] network = "Testnet"` or `"Regtest"`. |
| `bind`, `port` | Set one `[network] listen_addr = "IP:port"`; multiple `zcashd` binds are not a direct mapping. |
| `externalip` | Set `[network] external_addr` to the advertised socket address. |
| `addnode`, `seednode` | Use `[network] initial_mainnet_peers` or `initial_testnet_peers` as discovery seeds, not a persistent-peer guarantee. |
| `connect`, `discover`, proxy and relay-policy controls | No equivalent connect-only policy. Initial peers do not constrain later discovery; decisions remain in [#11027](https://github.com/ZcashFoundation/zebra/issues/11027). |
| `server`, `rpcbind`, `rpcport` | Set `[rpc] listen_addr`; the listener is disabled when unset. |
| `rpcuser`, `rpcpassword`, `rpcauth` | No static-credential mapping. Use Zebra's cookie authentication and an authenticated deployment boundary for remote access. |
| `rpccookiefile` | Set `[rpc] cookie_dir`; the filename is `.cookie`, not configurable separately. |
| `rpcallowip` | No source-address allowlist setting. Bind to loopback or enforce access at the deployment boundary. |
| `rpcthreads` | No equivalent thread-count control; `[rpc] parallel_cpu_threads` is deprecated and has no effect. |
| `mineraddress` | Set `[mining] miner_address` on Zebra, not the sidecar. |
| `blocknotify` | Set `[notify] block_notify_command`. |
| Wallet and ZMQ settings retained during migration | Keep them in the sidecar's `zcash.conf`; Zebra does not translate or forward them. |

When reusing sidecar data, [preserve its transaction-index setting](zcashd-compat-migration.md#2-write-the-config).

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
[zallet-semantics]: https://zcash.github.io/zallet/zcashd/json_rpc.html
[zallet-changelog]: https://github.com/zcash/zallet/blob/main/CHANGELOG.md
[indexer-proto]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-rpc/proto/indexer.proto
[zallet-rpc-source]: https://github.com/zcash/zallet/blob/v0.1.0-beta.3/zallet-core/src/components/json_rpc/methods.rs
