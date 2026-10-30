# Getting new work to mining pools

First [configure and sync Zebra for mining](mining.md), including the miner address and authenticated
RPC access. Then choose a new-work delivery method supported by your pool software, in this order:

1. **Long polling:** hold a `getblocktemplate` request until work changes.
2. **Block notifications:** trigger a template request when Zebra's best chain tip changes.
3. **Polling:** periodically request a template as a fallback or backstop.

With a polling interval of _I_, a new block arriving uniformly between polls leaves miners on stale
work for _I / 2_ on average, before RPC and job-distribution delays. For example, 500 ms polling adds
about 250 ms of stale work per block. Notifications remove that polling wait, not all delivery delay;
this is a latency estimate, not a guaranteed revenue gain.

## Prefer getblocktemplate long polling

The pool's template provider must explicitly implement long polling; receiving a `longpollid` does
not enable it automatically.

1. Fetch an initial template without a long-poll ID:

   ```json
   {"jsonrpc":"2.0","id":1,"method":"getblocktemplate","params":[{}]}
   ```

2. Publish the template to miners, then make another request with its returned `longpollid`:

   ```json
   {
     "jsonrpc": "2.0",
     "id": 2,
     "method": "getblocktemplate",
     "params": [{"longpollid": "<exact ID from the previous response>"}]
   }
   ```

3. Keep that request open. When it returns, process the template and repeat using its new ID.

Treat the ID as opaque: do not parse it, assume its length, or construct it yourself. Keep one
outstanding long poll per backend, with a separate connection or connection-pool capacity for block
submissions. Configure client and proxy timeouts for long-held requests, rather than a short ordinary
RPC timeout.

A response can reflect a new chain tip, changed transactions, or an expired timestamp range. Ensure
the pool forwards updated work to miners even when `previousblockhash` is unchanged. Honor
`submitold` when deciding whether to retain previous work; do not discard it merely because the
transaction selection changed. After a timeout or connection failure, reconnect with bounded retry
backoff, fetch a fresh template without an ID, and restart the long-poll loop.

## Use block notifications

### s-nomp commands

For a pool that supports a block-notification command but not long polling, add this to Zebra's
configuration and restart the node:

```toml
[notify]
block_notify_command = "node /path/to/s-nomp/scripts/cli.js blocknotify <coin> %s"
```

Before using this example, replace the script path and `<coin>` with the pool's configured coin name.
The CLI connects to s-nomp's local command listener by default, so this example assumes it is
reachable from the Zebra host. Ensure the service account running Zebra can execute Node.js and read
the script. The command runs through the system shell; keep its configuration under operator control.

Zebra substitutes the new best-tip block hash for `%s`. The pool uses the notification to request a
fresh template and distribute new jobs; the notification itself is not a template. Commands run
asynchronously, only when Zebra is close to the network tip. Delivery is best-effort: intermediate
tips can be skipped during rapid changes, and notifications are not a replayable event log.

Keep periodic polling enabled as a backstop for missed notifications or a restarted pool. Block
notifications only signal tip changes, so retain the pool's periodic job refresh for transaction-only
updates too. See the [s-nomp testnet guide](mining-testnet-s-nomp.md#important) for its compatibility and
payment limitations; this command does not make an unsupported pool version production-ready.

### ZMQ subscribers

Pools that subscribe to zcashd's ZMQ `hashblock` topic, such as Miningcore, can use Zebra's ZMQ
publisher. Build Zebra with `--features zmq`, then configure:

```toml
[notify]
zmqpubhashblock = "tcp://127.0.0.1:28332"
```

Notifications start when Zebra is close to the network tip. Delivery is best-effort: use the
sequence numbers to detect gaps and reconcile through RPC after gaps or reconnects. Keep periodic
polling as a backstop; coalesced intermediate tips do not necessarily create sequence gaps.

The endpoint has no authentication; expose it only to trusted clients. See
[ZMQ block notifications](mining.md#zmq-block-notifications) for framing and resource limits.

## Resolve equal-work tip races

When backends choose different tips with equal cumulative work, call `preciousblock` with the same
known, non-finalized tip hash on each backend to align the fleet:

```json
{"jsonrpc":"2.0","id":1,"method":"preciousblock","params":["<preferred non-finalized tip hash>"]}
```

Among equal-work tips the last call wins, but more-work chains always take precedence, so a call on a
block with less work than the best tip is a no-op. The preference covers only that block while it is
a tip: once a block is added on top of it, the extended chain no longer carries it. If the race
continues at the next height with equal work, call `preciousblock` again on each backend with your
new tip. Known non-tip or finalized blocks also return `null` without changing the preference;
unknown hashes return `Block not found`. Preferences are in memory only and are forgotten on restart.

## Keep mining and payouts separate

Zebra validates blocks and serves mining work; it is not a wallet. Point the pool's mining RPC
connection at Zebra, and its payout or payment-processor connection at a compatible wallet that
controls the reward address. Configure the same intended reward address on each mining backend.

Use Zebra's `validateaddress` or `z_validateaddress` to check address validity, not wallet ownership.
The transparent-address response omits `ismine`; a valid shielded-address response reports
`ismine: false`. Check ownership with the payout wallet instead.

### Wallet RPC compatibility

As of 27 October 2026, no released Zallet direct-state backend reads this Zebra's database; see
[the database compatibility table](zallet.md#keep-the-versions-in-step). The beta.3 Zaino backend
can fetch over JSON-RPC without reading that database, but that topology is untested here.
The payout RPC gaps below apply regardless of backend.

This inventory compares [s-nomp at `88187c9`][snomp-payments] and
[Miningcore at `a553f62`][miningcore-payments] with
[Zallet 0.1.0-beta.3 at `987382f`][zallet-rpcs] and the
[zcashd-compat sidecar](zcashd-compat.md), release `zebra-compat-v1.3.0`.

| Processor | Wallet calls | Zallet | Sidecar |
| --- | --- | --- | --- |
| Both | `gettransaction`, `z_getbalance` | Absent; use `z_viewtransaction`, `z_getbalanceforaccount` | Available; enable `allowdeprecated=z_getbalance` |
| Both | `z_shieldcoinbase`, `z_sendmany`, `z_getoperationresult` | Available; sending has [altered semantics][zallet-semantics] | Available for transparent/Sapling flows |
| s-nomp | `getbalance`, `listunspent`, `sendmany` | Absent; account balances, `z_listunspent`, `z_sendmany` replace them | Available |
| s-nomp | `validateaddress`, `z_validateaddress` | Transparent validation omits required `ismine`; shielded method absent | Available with ownership information |
| s-nomp | `getaccount` | Absent | Absent; this processor ignores its failed result |
| s-nomp | `z_getoperationstatus` | Available | Available |
| Miningcore | `listunspent` (fallback shielding), `walletpassphrase`, `walletlock` | First absent; lock/unlock available | Available; lock/unlock need an encrypted wallet, whose creation is experimental and disabled by default |

The processors also query chain RPCs: s-nomp uses `getblock`, `getmininginfo`, and
`getnetworkinfo`; Miningcore uses `getblockchaininfo`. Those are not Zallet wallet methods.
The sidecar serves them. Miningcore constructs its payout client from the first daemon entry;
it does not provide s-nomp's separate `paymentProcessing.daemon` connection.

Miningcore remains inspected-only. Its hard-coded `0.0001` ZEC fee failed a separate shielding
probe, and its sends need an explicit privacy policy for transparent recipients. Adapting its fee
selection and privacy policy remains untested. The tested s-nomp changes below do not enable
legacy privacy behavior.

### Tested sidecar payout path

An isolated Regtest run mined through s-nomp, recorded real miner shares in Redis, and let
`libs/paymentProcessor.js` shield rewards, unshield into its payout address, and pay the miner.
The test used:

| Component | Version or revision |
| --- | --- |
| Zebra | `7.0.0+210.gf94b0b5`, revision `f94b0b5e5819`, archive mode |
| Sidecar | Unmodified `zebra-compat-v1.3.0`, revision `5c11db4f4fbc` |
| s-nomp | `0.0.9`, base `88187c9`, with the changes below |
| node-stratum-pool | `0.2.0`, revision [`2a0572caa96b`](https://github.com/upbqdn/node-stratum-pool/commit/2a0572caa96b953a82fc481a7164a030e0c3e0f2), preserving Zebra's coinbase |
| nheqminer | Revision [`72e320aacaa4`](https://github.com/ZcashFoundation/nheqminer/commit/72e320aacaa49c9e170be62d28a4189957767d3c), two CPU threads, Equihash 200,9 |
| Runtime | Node.js `8.17.0`, Redis `7.2.6` |

Apply these changes to `libs/paymentProcessor.js`. They track the shielding operation ID, reserve
the configured unshielding fee and permit the transparent staging recipient:

```diff
@@ -275 +275 @@
-                    var opid = (result.response || result[0].response);
+                    var opid = (result.response || result[0].response).opid;
@@ -294 +294 @@
-        if ((zBalance - 10000) <= 0)
+        if ((zBalance - coinsToSatoshies(fee)) <= 0)
@@ -303 +303 @@
-        var amount = satoshisToCoins(zBalance - 10000);
+        var amount = satoshisToCoins(zBalance - coinsToSatoshies(fee));
@@ -305 +305,2 @@
-        var params = [poolOptions.zAddress, [{'address': poolOptions.tAddress, 'amount': amount}]];
+        var params = [poolOptions.zAddress, [{'address': poolOptions.tAddress, 'amount': amount}],
+            minConfShield, fee, 'AllowRevealedRecipients'];
```

1. Run Zebra and an [externally managed sidecar](zcashd-compat.md#running-externally-managed).
   Pin the sidecar's only peer with `connect=<Zebra-P2P-address>` and `listen=0`; keep both RPC
   endpoints on loopback with separate credentials. Set `[zcashd_compat] enabled = true` and
   `manage_zcashd = false` on Zebra. The sidecar synchronizes over P2P and does
   not read Zebra's database, so the database-format mismatch does not affect it.
2. Create two distinct wallet-controlled transparent addresses with `getnewaddress` and a Sapling
   address with `z_getnewaddress sapling`. Enable `allowdeprecated=getnewaddress`,
   `allowdeprecated=z_getnewaddress`, and `allowdeprecated=z_getbalance` in the sidecar's
   `zcash.conf`. Verify ownership with the wallet's `validateaddress` and `z_validateaddress`.
   Set the reward address as `[mining] miner_address` on Zebra and `address` in s-nomp; set
   the other transparent address as `tAddress` and the Sapling address as `zAddress`.
3. Point s-nomp's `daemons` at Zebra and `paymentProcessing.daemon` at the sidecar's authenticated
   RPC endpoint. Enable `paymentProcessing.enabled` and use `paymentMode: "prop"`,
   `minConf: 10`, `paymentInterval: 30`, `minimumPayment: 0.05`, and `walletInterval: 1`.
   In the coin profile, retain `requireShielding: true` and `txfee: 0.0004`.
   Keep `rewardRecipients: {}` and `poolHex: ""`: this Stratum adapter preserves Zebra's coinbase
   rather than inserting pool-fee outputs. The test used a share difficulty of `1 / 1024`,
   disabled the website and clustering, and enabled `cleanJobsOnTemplateUpdate` for nheqminer.
4. Connect the miner with its transparent payout address as the username. Let the processor
   observe maturity through the sidecar, complete shielding and unshielding, and call `sendmany`.
   Check its Redis payment record, confirm that transaction on Zebra, and verify the recipient's
   output amount. The configured fee covers the tested small withdrawal, not arbitrary batches;
   size fees under ZIP 317. The privacy policy reveals recipient addresses and amounts.

For reproduction, the run activated Overwinter through NU5 at height 1 on both nodes, left later
upgrades inactive, and used fresh state with no funding streams. Zebra used `network = "Regtest"`,
`rpc.debug_force_finished_sync = true`, `rpc.enable_cookie_auth = false` on loopback only, and
`mempool.debug_enable_at_height = 0`. The sidecar used `regtest=1` and six `nuparams` entries:
`5ba81b19:1`, `76b809bb:1`, `2bb40e60:1`, `f5b9230b:1`, `e9ff75a6:1`, and `c2d6d0b4:1`.
It also set `regtestshieldcoinbase=1`, `regtestacceptunvalidatedpow=1`, and `maxtipage=2000000000`.
These test-only auth, sync, and PoW settings must not be copied to public networks.

The pool mined height 1. After stopping the miner, Zebra generated 101 blocks to mature its reward,
then ten blocks for each processor-created transaction. The test waited for matching node tips and
the sidecar's `getblockchaininfo.fullyNotified` after each batch. Shielding spent the pool coinbase
at height 103; unshielding confirmed at 113; the processor's **6.2496 ZEC** miner payment confirmed
at 123 and had ten confirmations at 132. The Redis payment record credited the real height-1
round, and the recipient UTXO matched its amount. Additional generated rewards shared the wallet;
the test did not fabricate shares or issue shielding, unshielding, or payment calls itself.

This proves one worker's proportional accounting and scheduled payout, not production readiness.
It does not test multiple miners, pool fees, restart or reorg recovery, encrypted wallets,
production proof of work, or upgrade transitions. The legacy Node.js version is unsupported.
Keep payouts transparent/Sapling: the sidecar [never supports Ironwood and rejects Orchard from
NU6.3 onward](zcashd-compat.md#wallet-shielded-pool-support-orchard--ironwood).

[snomp-payments]: https://github.com/ZcashFoundation/s-nomp/blob/88187c9ea162ec2fa8542290166aaaa5b5e04b0a/libs/paymentProcessor.js
[miningcore-payments]: https://github.com/oliverw/miningcore/blob/a553f62301f44c6df80891e408b6526d1dd98692/src/Miningcore/Blockchain/Equihash/EquihashPayoutHandler.cs
[zallet-rpcs]: https://github.com/zcash/zallet/blob/987382f67e622915228686e9f956c6a9c9a7514c/book/src/zcashd/rpc_status.md
[zallet-semantics]: https://zcash.github.io/zallet/zcashd/json_rpc.html#z_sendmany

## Plan for restarts and replacement

- Keep a separately synced standby node with the same network and mining settings, its own state
  directory, and its own RPC endpoint and credentials. Test the pool's backend failover before an
  outage; a reachable RPC port alone does not establish that the node can provide current work.
- Before a planned restart, move mining traffic to the standby. After startup, wait for sync and a
  fresh successful template before returning traffic. Re-read RPC cookie credentials after a restart
  when cookie authentication is enabled, and restart long polling with a fresh template.
- After a pool restart, request current work immediately rather than waiting for the next block
  notification. On backend changes, discard the old backend's long-poll ID and start a new loop.
- Use the published [state snapshots](snapshots.md) to bootstrap a replacement faster than syncing
  from genesis. Follow that guide's network, database-version, checksum, and extraction instructions,
  then let the node catch up before serving miners. Snapshots require trusting their producer's
  validation; checksums detect transfer corruption, not a malicious snapshot. Downloading, extracting,
  and catching up still take time, so snapshots complement a warm standby rather than replace one.

## Monitor mining

Use the [mining metrics](metrics.md#mining-metrics) to watch template build time, new-tip to first-template
latency, submission outcomes, and mined-block announcement failures and latency for stale-block risk.
Announcement completion measures local writes, not peer receipt or network-wide propagation.
