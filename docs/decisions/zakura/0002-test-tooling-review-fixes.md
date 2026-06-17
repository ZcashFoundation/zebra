# Zakura P2P Test Tooling Review Fixes

Plan: `/home/evan/src/valar/art/inbox/zakura_p2p/01.b_test_tooling.md`

## Applied in This Fix

- Inbound delivery now has one production path: stream workers send frames into
  the bounded inbound queue, and the queue worker delivers to the configured
  `InboundSink`. This removes the previous synchronous side path and frame clone.
- `InboundRecorder` is documented and tested as a bounded lossy observation tap,
  with an explicit drop counter.
- `await_until` uses a single timeout and a final predicate check.
- `ZakuraTestNode::connect_native` waits on the supervisor watch channel instead
  of rebuilding subscriptions in a polling loop or fabricating timeout errors.
- The token bucket uses an injectable `Clock`; the testkit `TestClock` now
  drives the production rate-limit logic in unit tests.
- `HostilePeer::connect_native` uses the production native initiator handshake
  helper instead of duplicating valid control-handshake framing.
- Reserved builder hooks now fail loudly where behavior is unavailable:
  `enable_legacy_upgrade(true)` returns an error at spawn time.
- The codec load generator reports rates from measured elapsed time and marks its
  output mode as `frame_codec`.

## Not Applied

- The ignored native two-node mesh test is still not ready to unignore. Forced
  validation fails deterministically with `WaitError { description: "cluster peer
  set", timeout: 5s }`, showing that the current native harness path does not yet
  produce a stable bidirectional supervisor peer set. Unignoring it in this fix
  would add a known failing test; changing the connection lifecycle is a
  production `01.a` follow-up, not a review-only testkit patch.
- A true single-connection QUIC throughput bench remains blocked by the same
  native lifecycle issue. The existing bench/loadgen artifacts are intentionally
  limited to codec smoke coverage after this fix.
- Full metrics scraping/`await_at_least`, the complete hostile capability menu,
  and Docker e2e mesh assertions by deterministic node id remain incomplete. They
  need the stable native lifecycle and trace/metrics surfaces from later Zakura
  work rather than additional stubs in this review fix.
- No security review findings were found in the registered durable inputs for
  item `0026-review-security-01-b-r1`; there was no separate security finding to
  apply beyond the bounded-resource fixes above.
