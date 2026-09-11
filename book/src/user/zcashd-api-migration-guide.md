# zcashd API Migration Guide

`zcashd` is being retired. For every public API method it exposes, an operator
migrating away from it needs one question answered: **who owns this now?**

This page is that record. For each of the 135 RPC methods `zcashd` 6.20.0
registers, plus its ZMQ topics, REST interface, and notification hooks, it
names the component that owns the API method after `zcashd` retires — or states
that nobody does, and the API method goes away with `zcashd`.

It is a statement of ownership, not a feature comparison. "Zebra owns
`getblockchaininfo`" means Zebra is where that API method lives from now on; it
does not promise byte-identical responses. Where Zebra diverges from
`zcashd`, the Notes column says how.

## Where the boundary falls today

| Owner         | Methods | Share |
| ------------- | ------- | ----- |
| **Zallet**    | 42      | 31%   |
| **Nobody**    | 41      | 30%   |
| **Zebra**     | 36      | 27%   |
| **Undecided** | 9       | 7%    |
| **Zaino**     | 7       | 5%    |

## How to read the table

### Method

Links go to the [`zcashd` RPC reference](https://zcash.github.io/rpc/).
Methods `zcashd` keeps out of `help` are unlinked, because the generated
reference omits them too.

### Owner

Who owns the API method once `zcashd` is gone.

| Owner         | Meaning                                                                                                                                                                                                                           |
| ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Zebra**     | The validator node. Node-level chain, network, mempool, and mining API methods.                                                                                                                                                   |
| **Zallet**    | The [`zcashd` wallet replacement](https://github.com/zcash/wallet). Anything that needs keys or wallet state; per-method status follows [Zallet's JSON-RPC method status](https://zcash.github.io/zallet/zcashd/rpc_status.html). |
| **Zaino**     | The [indexer and wallet-facing service](https://github.com/zingolabs/zaino).                                                                                                                                                      |
| **Nobody**    | Explicit non-goal. No component will provide this method; it disappears with `zcashd`.                                                                                                                                            |
| **Undecided** | No owner assigned yet; the disposition is still open.                                                                                                                                                                             |

Owner names the component that _implements_ the API method. Zaino re-exposes many
Zebra-owned methods as pass-throughs for wallet clients; those rows say
**Zebra**, because that is where the implementation lives.

A marker on the owner records what the [`zcashd`-compat
sidecar](zcashd-compat.md) does with the method during a migration. The sidecar
is a **temporary bridge, not an owner**: an unmarked row buys migration time, it
does not remove the need to move to the owner named in the column.

| Marker   | Meaning                                                                                                                                                                                          |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| _(none)_ | The sidecar serves it with stock `zcashd` semantics.                                                                                                                                             |
| **†**    | The sidecar serves it but rejects some inputs — Ironwood always, and Orchard from NU6.3 on. See [Wallet shielded-pool support](zcashd-compat.md#wallet-shielded-pool-support-orchard--ironwood). |
| **\***   | Not registered in the sidecar build. The miner RPCs and `addnode` are removed so a misconfigured miner or peer setup fails loudly.                                                               |

### Status

Whether the API method is available **from the owner named in the previous
column** — not from `zcashd`.

| Status        | Meaning                                                                       |
| ------------- | ----------------------------------------------------------------------------- |
| **Done**      | The owner implements it and treats it as a supported API method.              |
| **Partial**   | Implemented, but diverges from `zcashd`. The Notes column says how.           |
| **Stub**      | Registered and returns success, but does nothing.                             |
| **Planned**   | Owner assigned, not shipped yet.                                              |
| **Undecided** | Owner assigned, but whether that owner will provide the method is still open. |
| **None**      | Not available and not planned. Pairs with Owner `Nobody` or `Undecided`.      |

> [!NOTE]
> The table might be outdated. If a method is in "Planned", "Undecided", or "None"
> and you need, double-check to see if it might be already available.

## RPC methods

### Control

| Method                                                                                | Owner                 | Status  | Notes                                                                                |
| ------------------------------------------------------------------------------------- | --------------------- | ------- | ------------------------------------------------------------------------------------ |
| [`getexperimentalfeatures`](https://zcash.github.io/rpc/getexperimentalfeatures.html) | Nobody                | None    |                                                                                      |
| [`getinfo`](https://zcash.github.io/rpc/getinfo.html)                                 | Zebra                 | Partial | No `timeoffset`. The wallet fields are zcashd-wallet-only and never apply.           |
| [`getmemoryinfo`](https://zcash.github.io/rpc/getmemoryinfo.html)                     | Nobody                | None    |                                                                                      |
| [`help`](https://zcash.github.io/rpc/help.html)                                       | [Zallet][zallet-help] | Done    | Zallet serves `help` for its own RPC; Zebra does not.                                |
| [`setlogfilter`](https://zcash.github.io/rpc/setlogfilter.html)                       | Nobody                | None    |                                                                                      |
| [`stop`](https://zcash.github.io/rpc/stop.html)                                       | Zebra                 | Partial | Regtest only. Returns `Zebra server stopping`, not zcashd's `Zcash server stopping`. |

### Blockchain

| Method                                                                          | Owner     | Status  | Notes                                                                                                                                                                                               |
| ------------------------------------------------------------------------------- | --------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`getbestblockhash`](https://zcash.github.io/rpc/getbestblockhash.html)         | Zebra     | Done    |                                                                                                                                                                                                     |
| [`getblock`](https://zcash.github.io/rpc/getblock.html)                         | Zebra     | Partial | No `authdataroot`, `chainhistoryroot`, `chainwork`, or `anchor`. The `verbosity` argument must be a number, where zcashd also accepts a bool. Adds `nTx` and an `ironwood` entry in `trees`.        |
| [`getblockchaininfo`](https://zcash.github.io/rpc/getblockchaininfo.html)       | Zebra     | Partial | No `initial_block_download_complete` or `softforks`.                                                                                                                                                |
| [`getblockcount`](https://zcash.github.io/rpc/getblockcount.html)               | Zebra     | Done    |                                                                                                                                                                                                     |
| [`getblockdeltas`](https://zcash.github.io/rpc/getblockdeltas.html)             | Zaino     | Planned |                                                                                                                                                                                                     |
| [`getblockhash`](https://zcash.github.io/rpc/getblockhash.html)                 | Zebra     | Done    |                                                                                                                                                                                                     |
| [`getblockhashes`](https://zcash.github.io/rpc/getblockhashes.html)             | Undecided | None    |                                                                                                                                                                                                     |
| [`getblockheader`](https://zcash.github.io/rpc/getblockheader.html)             | Zebra     | Partial | No `chainwork`, which is undocumented in zcashd. Adds `blockcommitments`.                                                                                                                           |
| [`getchaintips`](https://zcash.github.io/rpc/getchaintips.html)                 | Zaino     | Planned |                                                                                                                                                                                                     |
| [`getdifficulty`](https://zcash.github.io/rpc/getdifficulty.html)               | Zebra     | Partial | Computed from the high 128 bits of the expanded difficulty instead of zcashd's `f64` division; the two agree to `f64` precision. Errors where zcashd returns `1.0` on a chain too short to measure. |
| [`getmempoolinfo`](https://zcash.github.io/rpc/getmempoolinfo.html)             | Zebra     | Partial | The regtest-only key is spelled `fully_notified`; zcashd spells it `fullyNotified`.                                                                                                                 |
| [`getrawmempool`](https://zcash.github.io/rpc/getrawmempool.html)               | Zebra     | Done    |                                                                                                                                                                                                     |
| [`getspentinfo`](https://zcash.github.io/rpc/getspentinfo.html)                 | Zaino     | Planned |                                                                                                                                                                                                     |
| [`gettxout`](https://zcash.github.io/rpc/gettxout.html)                         | Zebra     | Done    |                                                                                                                                                                                                     |
| [`gettxoutproof`](https://zcash.github.io/rpc/gettxoutproof.html)               | Zaino     | Planned |                                                                                                                                                                                                     |
| [`gettxoutsetinfo`](https://zcash.github.io/rpc/gettxoutsetinfo.html)           | Zaino     | Planned |                                                                                                                                                                                                     |
| [`verifychain`](https://zcash.github.io/rpc/verifychain.html)                   | Nobody    | None    |                                                                                                                                                                                                     |
| [`verifytxoutproof`](https://zcash.github.io/rpc/verifytxoutproof.html)         | Zaino     | Planned |                                                                                                                                                                                                     |
| [`z_getsubtreesbyindex`](https://zcash.github.io/rpc/z_getsubtreesbyindex.html) | Zebra     | Done    |                                                                                                                                                                                                     |
| [`z_gettreestate`](https://zcash.github.io/rpc/z_gettreestate.html)             | Zebra     | Partial | Each pool reports `finalState` only — no `finalRoot` or `skipHash`. `sprout` is omitted when empty, where zcashd always emits it. Adds `ironwood`.                                                  |

### Address index

| Method                                                                    | Owner  | Status  | Notes |
| ------------------------------------------------------------------------- | ------ | ------- | ----- |
| [`getaddressbalance`](https://zcash.github.io/rpc/getaddressbalance.html) | Zebra  | Done    |       |
| [`getaddressdeltas`](https://zcash.github.io/rpc/getaddressdeltas.html)   | Zaino  | Planned |       |
| [`getaddressmempool`](https://zcash.github.io/rpc/getaddressmempool.html) | Nobody | None    |       |
| [`getaddresstxids`](https://zcash.github.io/rpc/getaddresstxids.html)     | Zebra  | Done    |       |
| [`getaddressutxos`](https://zcash.github.io/rpc/getaddressutxos.html)     | Zebra  | Done    |       |

### Mining

| Method                                                                            | Owner   | Status  | Notes                                                                                                                                                                                                    |
| --------------------------------------------------------------------------------- | ------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`getblocksubsidy`](https://zcash.github.io/rpc/getblocksubsidy.html)             | Zebra   | Done    |                                                                                                                                                                                                          |
| [`getblocktemplate`](https://zcash.github.io/rpc/getblocktemplate.html)           | Zebra\* | Done    | Superset: adds `maxtime` and `submitold`. `capabilities` and `mutable` match zcashd, and long polling and proposal mode are both implemented.                                                            |
| [`getlocalsolps`](https://zcash.github.io/rpc/getlocalsolps.html)                 | Nobody  | None    |                                                                                                                                                                                                          |
| [`getmininginfo`](https://zcash.github.io/rpc/getmininginfo.html)                 | Zebra   | Partial | No `difficulty`, `errors`, `errorstimestamp`, `genproclimit`, `localsolps`, `pooledtx`, or `generate`.                                                                                                   |
| [`getnetworkhashps`](https://zcash.github.io/rpc/getnetworkhashps.html)           | Zebra   | Done    | Alias for `getnetworksolps`, as in zcashd, where it is deprecated. Planned for deprecation in Zebra too.                                                                                                 |
| [`getnetworksolps`](https://zcash.github.io/rpc/getnetworksolps.html)             | Zebra   | Done    |                                                                                                                                                                                                          |
| [`prioritisetransaction`](https://zcash.github.io/rpc/prioritisetransaction.html) | Nobody  | None    |                                                                                                                                                                                                          |
| [`submitblock`](https://zcash.github.io/rpc/submitblock.html)                     | Zebra\* | Partial | Returns only `null`, `duplicate`, or `rejected`. zcashd also returns `duplicate-invalid`, `duplicate-inconclusive`, `inconclusive`, and the specific BIP-22 reject reason in place of a bare `rejected`. |

### Generating

| Method                                                        | Owner    | Status | Notes                       |
| ------------------------------------------------------------- | -------- | ------ | --------------------------- |
| [`generate`](https://zcash.github.io/rpc/generate.html)       | Zebra\*  | Done   | Regtest only, as in zcashd. |
| [`getgenerate`](https://zcash.github.io/rpc/getgenerate.html) | Nobody\* | None   |                             |
| [`setgenerate`](https://zcash.github.io/rpc/setgenerate.html) | Nobody\* | None   |                             |

### Network

| Method                                                                      | Owner     | Status  | Notes                                                                                                                     |
| --------------------------------------------------------------------------- | --------- | ------- | ------------------------------------------------------------------------------------------------------------------------- |
| [`addnode`](https://zcash.github.io/rpc/addnode.html)                       | Zebra\*   | Partial | Regtest only, and only the `add` command.                                                                                 |
| [`clearbanned`](https://zcash.github.io/rpc/clearbanned.html)               | Undecided | None    |                                                                                                                           |
| [`disconnectnode`](https://zcash.github.io/rpc/disconnectnode.html)         | Undecided | None    |                                                                                                                           |
| [`getaddednodeinfo`](https://zcash.github.io/rpc/getaddednodeinfo.html)     | Undecided | None    |                                                                                                                           |
| [`getconnectioncount`](https://zcash.github.io/rpc/getconnectioncount.html) | Undecided | None    |                                                                                                                           |
| [`getdeprecationinfo`](https://zcash.github.io/rpc/getdeprecationinfo.html) | Zebra     | Partial | Only `end_of_service`. No `version`, `subversion`, `deprecated_features`, or `disabled_features`.                         |
| [`getnettotals`](https://zcash.github.io/rpc/getnettotals.html)             | Undecided | None    |                                                                                                                           |
| [`getnetworkinfo`](https://zcash.github.io/rpc/getnetworkinfo.html)         | Zebra     | Partial | No `warningstimestamp`.                                                                                                   |
| [`getpeerinfo`](https://zcash.github.io/rpc/getpeerinfo.html)               | Zebra     | Partial | 10 of zcashd's 24 per-peer fields; Zebra's peer set does not track the rest. Adds `connection_state`.                     |
| [`listbanned`](https://zcash.github.io/rpc/listbanned.html)                 | Undecided | None    |                                                                                                                           |
| [`ping`](https://zcash.github.io/rpc/ping.html)                             | Zebra     | Stub    | Does not explicitly send pings. `getpeerinfo`'s `pingtime` and `pingwait` come from the last periodic ping that was sent. |
| [`setban`](https://zcash.github.io/rpc/setban.html)                         | Undecided | None    |                                                                                                                           |

### Raw transactions

| Method                                                                          | Owner                                 | Status  | Notes                                                                                                                         |
| ------------------------------------------------------------------------------- | ------------------------------------- | ------- | ----------------------------------------------------------------------------------------------------------------------------- |
| [`createrawtransaction`](https://zcash.github.io/rpc/createrawtransaction.html) | Nobody                                | None    | Zallet exposes PCZTs instead.                                                                                                 |
| [`decoderawtransaction`](https://zcash.github.io/rpc/decoderawtransaction.html) | [Zallet][zallet-decoderawtransaction] | Done    |                                                                                                                               |
| [`decodescript`](https://zcash.github.io/rpc/decodescript.html)                 | [Zallet][zallet-decodescript]         | Done    |                                                                                                                               |
| [`fundrawtransaction`](https://zcash.github.io/rpc/fundrawtransaction.html)     | Nobody                                | None    | Zallet exposes PCZTs instead.                                                                                                 |
| [`getrawtransaction`](https://zcash.github.io/rpc/getrawtransaction.html)       | Zebra                                 | Partial | `vout` omits `valueSat`, and the `-spentindex` fields (`spentTxId`, `spentIndex`, `spentHeight`) are absent. Adds `ironwood`. |
| [`sendrawtransaction`](https://zcash.github.io/rpc/sendrawtransaction.html)     | Zebra                                 | Partial | `allowhighfees` is accepted and ignored.                                                                                      |
| [`signrawtransaction`](https://zcash.github.io/rpc/signrawtransaction.html)     | Nobody                                | None    | Zallet exposes PCZTs instead.                                                                                                 |

### Util

| Method                                                                    | Owner                          | Status  | Notes                                                                 |
| ------------------------------------------------------------------------- | ------------------------------ | ------- | --------------------------------------------------------------------- |
| [`createmultisig`](https://zcash.github.io/rpc/createmultisig.html)       | [Zallet][zallet-status]        | Planned |                                                                       |
| [`validateaddress`](https://zcash.github.io/rpc/validateaddress.html)     | Zebra                          | Partial | No `scriptPubKey`. Adds `isscript`.                                   |
| [`verifymessage`](https://zcash.github.io/rpc/verifymessage.html)         | [Zallet][zallet-verifymessage] | Done    |                                                                       |
| [`z_validateaddress`](https://zcash.github.io/rpc/z_validateaddress.html) | Zebra                          | Partial | No `type`, `payingkey`, or `transmissionkey` — all Sprout-era fields. |

### Wallet

| Method                                                                                    | Owner                                   | Status    | Notes                                                                                        |
| ----------------------------------------------------------------------------------------- | --------------------------------------- | --------- | -------------------------------------------------------------------------------------------- |
| [`addmultisigaddress`](https://zcash.github.io/rpc/addmultisigaddress.html)               | [Zallet][zallet-status]                 | Planned   |                                                                                              |
| [`backupwallet`](https://zcash.github.io/rpc/backupwallet.html)                           | Nobody                                  | None      | Zallet may provide this as a CLI command rather than an RPC.                                 |
| [`dumpprivkey`](https://zcash.github.io/rpc/dumpprivkey.html)                             | Nobody                                  | None      |                                                                                              |
| [`encryptwallet`](https://zcash.github.io/rpc/encryptwallet.html)                         | Nobody                                  | None      | Key material is always encrypted from wallet setup; use `walletpassphrase`/`walletlock`.     |
| [`getbalance`](https://zcash.github.io/rpc/getbalance.html)                               | Nobody                                  | None      | Use `z_getbalanceforaccount` instead.                                                        |
| [`getnewaddress`](https://zcash.github.io/rpc/getnewaddress.html)                         | Nobody                                  | None      | Use `z_getnewaccount` + `z_getaddressforaccount`.                                            |
| [`getrawchangeaddress`](https://zcash.github.io/rpc/getrawchangeaddress.html)             | Nobody                                  | None      | Zallet derives change addresses internally.                                                  |
| [`getreceivedbyaddress`](https://zcash.github.io/rpc/getreceivedbyaddress.html)           | [Zallet][zallet-status]                 | Planned   |                                                                                              |
| [`gettransaction`](https://zcash.github.io/rpc/gettransaction.html)                       | Nobody                                  | None      | Use `z_viewtransaction` instead.                                                             |
| [`getunconfirmedbalance`](https://zcash.github.io/rpc/getunconfirmedbalance.html)         | [Zallet][zallet-status]                 | Planned   |                                                                                              |
| [`getwalletinfo`](https://zcash.github.io/rpc/getwalletinfo.html)                         | [Zallet][zallet-getwalletinfo]          | Partial   | Balance fields are not populated; several other fields are still placeholders.               |
| [`importaddress`](https://zcash.github.io/rpc/importaddress.html)                         | Nobody                                  | None      | Use the `zallet import-address` CLI command, or `z_importaddress`.                           |
| [`importprivkey`](https://zcash.github.io/rpc/importprivkey.html)                         | [Zallet][zallet-status]                 | Planned   |                                                                                              |
| [`importpubkey`](https://zcash.github.io/rpc/importpubkey.html)                           | Nobody                                  | None      | Use `z_importaddress`.                                                                       |
| [`importwallet`](https://zcash.github.io/rpc/importwallet.html)                           | Nobody                                  | None      | Use `z_importkey` per key, or `zallet migrate-zcashd-wallet`.                                |
| [`keypoolrefill`](https://zcash.github.io/rpc/keypoolrefill.html)                         | Nobody                                  | None      | Zallet has no key pool.                                                                      |
| [`listaddresses`](https://zcash.github.io/rpc/listaddresses.html)                         | [Zallet][zallet-listaddresses]          | Done      |                                                                                              |
| [`listaddressgroupings`](https://zcash.github.io/rpc/listaddressgroupings.html)           | Nobody                                  | None      |                                                                                              |
| [`listlockunspent`](https://zcash.github.io/rpc/listlockunspent.html)                     | [Zallet][zallet-status]                 | Planned   |                                                                                              |
| [`listreceivedbyaddress`](https://zcash.github.io/rpc/listreceivedbyaddress.html)         | [Zallet][zallet-status]                 | Planned   |                                                                                              |
| [`listsinceblock`](https://zcash.github.io/rpc/listsinceblock.html)                       | [Zallet][zallet-status]                 | Planned   |                                                                                              |
| [`listtransactions`](https://zcash.github.io/rpc/listtransactions.html)                   | [Zallet][zallet-status]                 | Planned   | Available today as the account-scoped `z_listtransactions`.                                  |
| [`listunspent`](https://zcash.github.io/rpc/listunspent.html)                             | Nobody                                  | None      | Use `z_listunspent` instead.                                                                 |
| [`lockunspent`](https://zcash.github.io/rpc/lockunspent.html)                             | [Zallet][zallet-status]                 | Planned   |                                                                                              |
| [`sendmany`](https://zcash.github.io/rpc/sendmany.html)                                   | Nobody                                  | None      | Use `z_sendmany` or `z_sendfromaccount`.                                                     |
| [`sendtoaddress`](https://zcash.github.io/rpc/sendtoaddress.html)                         | Nobody                                  | None      | Use `z_sendfromaccount`; `z_sendmany` covers most uses too.                                  |
| [`settxfee`](https://zcash.github.io/rpc/settxfee.html)                                   | Nobody                                  | None      | ZIP 317 fees are always used.                                                                |
| [`signmessage`](https://zcash.github.io/rpc/signmessage.html)                             | [Zallet][zallet-signmessage]            | Done      |                                                                                              |
| [`walletconfirmbackup`](https://zcash.github.io/rpc/walletconfirmbackup.html)             | Nobody                                  | None      | Internal to zcashd; use the `zallet confirm-backup` command.                                 |
| [`walletlock`](https://zcash.github.io/rpc/walletlock.html)                               | [Zallet][zallet-walletlock]             | Done      | Unlocks and re-locks the key store.                                                          |
| [`walletpassphrase`](https://zcash.github.io/rpc/walletpassphrase.html)                   | [Zallet][zallet-walletpassphrase]       | Done      | Unlocks and re-locks the key store.                                                          |
| [`walletpassphrasechange`](https://zcash.github.io/rpc/walletpassphrasechange.html)       | [Zallet][zallet-status]                 | Undecided |                                                                                              |
| [`z_converttex`](https://zcash.github.io/rpc/z_converttex.html)                           | [Zallet][zallet-z_converttex]           | Done      |                                                                                              |
| [`z_exportkey`](https://zcash.github.io/rpc/z_exportkey.html)                             | [Zallet][zallet-z_exportkey]            | Done      |                                                                                              |
| [`z_exportviewingkey`](https://zcash.github.io/rpc/z_exportviewingkey.html)               | [Zallet][zallet-z_exportviewingkey]     | Done      |                                                                                              |
| [`z_exportwallet`](https://zcash.github.io/rpc/z_exportwallet.html)                       | [Zallet][zallet-status]                 | Planned   | Planned as a ZeWIF export, likely a CLI command.                                             |
| [`z_getaddressforaccount`](https://zcash.github.io/rpc/z_getaddressforaccount.html)       | [Zallet][zallet-z_getaddressforaccount] | Done      |                                                                                              |
| [`z_getbalance`](https://zcash.github.io/rpc/z_getbalance.html)                           | Nobody                                  | None      | Use `z_getbalanceforaccount` instead.                                                        |
| [`z_getbalanceforaccount`](https://zcash.github.io/rpc/z_getbalanceforaccount.html)       | [Zallet][zallet-z_getbalanceforaccount] | Done      |                                                                                              |
| [`z_getbalanceforviewingkey`](https://zcash.github.io/rpc/z_getbalanceforviewingkey.html) | Nobody                                  | None      | Imported viewing keys get accounts, so `z_getbalanceforaccount` covers them.                 |
| [`z_getmigrationstatus`](https://zcash.github.io/rpc/z_getmigrationstatus.html)           | Nobody                                  | None      | No Sprout support; may be revisited for a future pool migration.                             |
| [`z_getnewaccount`](https://zcash.github.io/rpc/z_getnewaccount.html)                     | [Zallet][zallet-z_getnewaccount]        | Done      |                                                                                              |
| [`z_getnewaddress`](https://zcash.github.io/rpc/z_getnewaddress.html)                     | Nobody                                  | None      | Use `z_getnewaccount` + `z_getaddressforaccount`.                                            |
| [`z_getnotescount`](https://zcash.github.io/rpc/z_getnotescount.html)                     | [Zallet][zallet-z_getnotescount]        | Done      |                                                                                              |
| [`z_getoperationresult`](https://zcash.github.io/rpc/z_getoperationresult.html)           | [Zallet][zallet-z_getoperationresult]   | Done      |                                                                                              |
| [`z_getoperationstatus`](https://zcash.github.io/rpc/z_getoperationstatus.html)           | [Zallet][zallet-z_getoperationstatus]   | Done      |                                                                                              |
| [`z_gettotalbalance`](https://zcash.github.io/rpc/z_gettotalbalance.html)                 | [Zallet][zallet-z_gettotalbalance]      | Done      | Deprecated; `include_watchonly = false` is not honored yet. Prefer `z_getbalanceforaccount`. |
| [`z_importkey`](https://zcash.github.io/rpc/z_importkey.html)                             | [Zallet][zallet-z_importkey]            | Done      | Sapling extended spending keys only.                                                         |
| [`z_importviewingkey`](https://zcash.github.io/rpc/z_importviewingkey.html)               | [Zallet][zallet-z_importviewingkey]     | Done      |                                                                                              |
| [`z_importwallet`](https://zcash.github.io/rpc/z_importwallet.html)                       | Nobody                                  | None      | Use `z_importkey` per key, or `zallet migrate-zcashd-wallet`.                                |
| [`z_listaccounts`](https://zcash.github.io/rpc/z_listaccounts.html)                       | [Zallet][zallet-z_listaccounts]         | Done      |                                                                                              |
| [`z_listaddresses`](https://zcash.github.io/rpc/z_listaddresses.html)                     | Nobody                                  | None      | Use `listaddresses` instead.                                                                 |
| [`z_listoperationids`](https://zcash.github.io/rpc/z_listoperationids.html)               | [Zallet][zallet-z_listoperationids]     | Done      |                                                                                              |
| [`z_listreceivedbyaddress`](https://zcash.github.io/rpc/z_listreceivedbyaddress.html)     | [Zallet][zallet-status]                 | Planned   |                                                                                              |
| [`z_listunifiedreceivers`](https://zcash.github.io/rpc/z_listunifiedreceivers.html)       | Zebra                                   | Done      |                                                                                              |
| [`z_listunspent`](https://zcash.github.io/rpc/z_listunspent.html)                         | [Zallet][zallet-z_listunspent]          | Done      |                                                                                              |
| [`z_mergetoaddress`](https://zcash.github.io/rpc/z_mergetoaddress.html)                   | [Zallet][zallet-status]†                | Planned   |                                                                                              |
| [`z_sendmany`](https://zcash.github.io/rpc/z_sendmany.html)                               | [Zallet][zallet-z_sendmany]†            | Done      |                                                                                              |
| [`z_setmigration`](https://zcash.github.io/rpc/z_setmigration.html)                       | Nobody                                  | None      | No Sprout support; may be revisited for a future pool migration.                             |
| [`z_shieldcoinbase`](https://zcash.github.io/rpc/z_shieldcoinbase.html)                   | [Zallet][zallet-z_shieldcoinbase]†      | Done      |                                                                                              |
| [`z_viewtransaction`](https://zcash.github.io/rpc/z_viewtransaction.html)                 | [Zallet][zallet-z_viewtransaction]      | Done      |                                                                                              |
| [`zcbenchmark`](https://zcash.github.io/rpc/zcbenchmark.html)                             | Nobody                                  | None      |                                                                                              |
| [`zcsamplejoinsplit`](https://zcash.github.io/rpc/zcsamplejoinsplit.html)                 | Nobody                                  | None      | No Sprout support.                                                                           |

### Disclosure

| Method                                                                                        | Owner  | Status | Notes |
| --------------------------------------------------------------------------------------------- | ------ | ------ | ----- |
| [`z_getpaymentdisclosure`](https://zcash.github.io/rpc/z_getpaymentdisclosure.html)           | Nobody | None   |       |
| [`z_validatepaymentdisclosure`](https://zcash.github.io/rpc/z_validatepaymentdisclosure.html) | Nobody | None   |       |

### Hidden

| Method                     | Owner                   | Status    | Notes                                                           |
| -------------------------- | ----------------------- | --------- | --------------------------------------------------------------- |
| `dumpwallet`               | Nobody                  | None      | Already removed from zcashd; a ZeWIF export is planned instead. |
| `invalidateblock`          | Zebra                   | Done      |                                                                 |
| `reconsiderblock`          | Zebra                   | Partial   | Returns the reconsidered block hashes; zcashd returns null.     |
| `resendwallettransactions` | [Zallet][zallet-status] | Undecided |                                                                 |
| `setmocktime`              | Undecided               | None      |                                                                 |

## Zebra-only methods

API methods Zebra adds that `zcashd` never had. Listed so this page describes the
whole RPC boundary, not only a `zcashd` subset. None of them exist in the
sidecar, so all four carry the `*` marker.

| Method                      | Owner   | Status | Notes                                                                                                                                                                    |
| --------------------------- | ------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `getbestblockheightandhash` | Zebra\* | Done   | Tip height and hash in one call, so a caller cannot read a torn pair across two requests.                                                                                |
| `getstandardfee`            | Zebra\* | Done   | The ZIP-317 conventional fee.                                                                                                                                            |
| `generatetoaddress`         | Zebra\* | Done   | Regtest only. Like `generate`, but pays the coinbase to a given address instead of the configured `mining.miner_address`, which lets one node fund several test wallets. |
| `rpc.discover`              | Zebra\* | Done   | OpenRPC schema for every Zebra RPC method.                                                                                                                               |

## Notification transports

`zcashd` publishes chain and mempool events over ZMQ. Zebra does not implement
ZMQ. It exposes an equivalent gRPC streaming service — the **indexer service**
(`zebra-rpc/proto/indexer.proto`) — which is what Zaino consumes.

The ZMQ transport itself is a **non-goal for Zebra**: the events survive the
migration, the wire format does not.

| `zcashd` topic    | Owner  | Status | Zebra equivalent                                   |
| ----------------- | ------ | ------ | -------------------------------------------------- |
| `zmqpubhashblock` | Nobody | None   | Indexer `ChainTipChange` stream (hash and height). |
| `zmqpubrawblock`  | Nobody | None   | Indexer `NonFinalizedStateChange` stream.          |
| `zmqpubhashtx`    | Nobody | None   | Indexer `MempoolChange` stream.                    |
| `zmqpubrawtx`     | Nobody | None   | Indexer `MempoolChange` plus `getrawtransaction`.  |

## Other integration points

| Integration point                   | Owner    | Status    | Notes                                                                                                            |
| ----------------------------------- | -------- | --------- | ---------------------------------------------------------------------------------------------------------------- |
| REST interface (`-rest`, `/rest/*`) | Nobody   | None      | Non-goal. JSON-RPC is the supported HTTP interface.                                                              |
| `-blocknotify` hook                 | Nobody   | None      | Non-goal: shelling out once per block does not fit Zebra's async model. Use the indexer `ChainTipChange` stream. |
| `-walletnotify` hook                | Zallet   | Undecided | A wallet concern, so it follows the wallet to Zallet.                                                            |
| `-alertnotify` hook                 | Nobody\* | None      | The Zcash alert system is retired; `zcashd` no longer sends alerts.                                              |

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
