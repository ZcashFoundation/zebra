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

## Use block notifications with s-nomp

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

## Keep mining and payouts separate

Zebra validates blocks and serves mining work; it is not a wallet. Point the pool's mining RPC
connection at Zebra, and its payout or payment-processor connection at a compatible wallet that
controls the reward address. Configure the same intended reward address on each mining backend.

Use `validateaddress` or `z_validateaddress` to check address validity, not wallet ownership. The
transparent-address response omits `ismine`; a valid `z_validateaddress` response reports
`ismine: false`. Neither proves that the payout wallet can spend the rewards. Check ownership with
that wallet and verify the pool's payout integration separately before production use.

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
