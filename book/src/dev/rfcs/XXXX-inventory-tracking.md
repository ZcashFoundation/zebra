- Feature Name: `inventory_tracking`
- Start Date: 2020-08-25
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/0000)
- Zebra Issue: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/issues/0000)

# Summary
[summary]: #summary

The Bitcoin network protocol used by Zcash allows nodes to advertise data
(inventory items) for download by other peers.  This RFC describes how we track
and use this information.

# Motivation
[motivation]: #motivation

In order to participate in the network, we need to be able to fetch new data
that our peers notify us about.  Because our network stack abstracts away
individual peer connections, and load-balances over available peers, we need a
way to direct requests for new inventory only to peers that advertised to us
that they have it.

# Definitions
[definitions]: #definitions

- Inventory item: either a block or transaction.
- Inventory hash: the hash of an inventory item, represented by the
  [`InventoryHash`](https://doc-internal.zebra.zfnd.org/zebra_network/protocol/external/inv/enum.InventoryHash.html)
  type.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

The Bitcoin network protocol used by Zcash provides a mechanism for nodes to
gossip blockchain data to each other.  This mechanism is used to distribute
(mined) blocks and (unmined) transactions through the network.  Nodes can
advertise data available in their inventory by sending an `inv` message
containing the hashes and types of those data items.  After receiving an `inv`
message advertising data, a node can determine whether to download it.

This poses a challenge for our network stack, which goes to some effort to
abstract away details of individual peers and encapsulate all peer connections
behind a single request/response interface representing "the network".
Currently, the peer set tracks readiness of all live peers, reports readiness
if at least one peer is ready, and routes requests across ready peers randomly
using the ["power of two choices"][p2c] algorithm.

However, while this works well for data that is already distributed across the
network (e.g., existing blocks) it will not work well for fetching data
*during* distribution across the network.  If a peer informs us of some new
data, and we attempt to download it from a random, unrelated peer, we will
likely fail.  Instead, we track recent inventory advertisements, and make a
best-effort attempt to route requests to peers who advertised that inventory.

[p2c]: https://www.eecs.harvard.edu/~michaelm/postscripts/mythesis.pdf

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

The inventory tracking system has several components:

1.  A registration hook that monitors incoming messages for inventory advertisements;
2.  An inventory registry that tracks inventory presence by peer;
3.  Routing logic that uses the inventory registry to appropriately route requests.

The first two components have fairly straightforward design decisions, but
the third has considerably less obvious choices and tradeoffs.

## Inventory Monitoring

Zebra uses Tokio's codec mechanism to transform a byte-oriented I/O interface
into a `Stream` and `Sink` for incoming and outgoing messages.  These are
passed to the peer connection state machine, which is written generically over
any `Stream` and `Sink`.  This construction makes it easy to "tap" the sequence
of incoming messages using `.then` and `.with` stream and sink combinators.

We already do this to record Prometheus metrics on message rates as well as to
report message timestamps used for liveness checks and last-seen address book
metadata.  The message timestamp mechanism is a good example to copy.  The
handshake logic instruments the incoming message stream with a closure that
captures a sender handle for a [mpsc] channel with a large buffer (currently 100
timestamp entries). The receiver handle is owned by a separate task that shares
an `Arc<Mutex<AddressBook>>` with other parts of the application.  This task
waits for new timestamp entries, acquires a lock on the address book, and
updates the address book.  This ensures that timestamp updates are queued
asynchronously, without lock contention.

Unlike the address book, we don't need to share the inventory data with other
parts of the application, so it can be owned exclusively by the peer set.  This
means that no lock is necessary, and the peer set can process advertisements in
its `poll_ready` implementation.  This method may be called infrequently, which
could cause the channel to fill.  However, because inventory advertisements are
time-limited, in the sense that they're only useful before some item is fully
distributed across the network, it's safe to handle excess entries by dropping
them.  This behavior is provided by a [broadcast]/mpmc channel, which can be
used in place of an mpsc channel.

[mpsc]: https://docs.rs/tokio/0.2.22/tokio/sync/mpsc/index.html
[broadcast]: https://docs.rs/tokio/0.2.22/tokio/sync/broadcast/index.html

An inventory advertisement is an `(InventoryHash, SocketAddr)` pair.  The
stream hook should check whether an incoming message is an `inv` message with
only a small number (e.g., 1) inventory entries.  If so, it should extract the
hash for each item and send it through the channel.  Otherwise, it should
ignore the message contents.  Why?  Because `inv` messages are also sent in
response to queries, such as when we request subsequent block hashes, and in
that case we want to assume that the inventory is generally available rather
than restricting downloads to a single peer.  However, items are usually
gossiped individually (or potentially in small chunks; `zcashd` has an internal
`inv` buffer subject to race conditions), so choosing a small bound such as 1
is likely to work as a heuristic for when we should assume that advertised
inventory is not yet generally available.

## Inventory Registry

The peer set's `poll_ready` implementation should extract all available
`(InventoryHash, SocketAddr)` pairs from the channel, and log a warning event
if the receiver is lagging.  The channel should be configured with a generous
buffer size (such as 100) so that this is unlikely to happen in normal
circumstances.  These pairs should be fed into an `InventoryRegistry` structure
along these lines:

```rust
struct InventoryRegistry(HashMap<InventoryHash, HashSet<SocketAddr>>);

impl InventoryRegistry {
    pub fn register(&mut self, item: InventoryHash, addr: SocketAddr) {
	self.0.entry(item).or_insert(HashSet::new).insert(addr);
    }

    pub fn peers(&self, item: InventoryHash) -> Option<&HashSet<SocketAddr>> {
	self.0.get(item)
    }
}
```

This API does not provide a way to remove inventory advertisements.  Instead,
the peer set should maintain a `tokio::time::Interval` with some interval
parameter, and check in `poll_ready` whether the timer has elapsed.  If so, it
should use `std::mem::take` to extract and drop the current inventory registry.
This is much simpler than trying to manage the lifetime of individual
advertisements.  The downside is that if we drop the inventory registry before
we look up some data, we may not be able to find it until it's widely
distributed.  To minimize the chance of this happening, the timer check should
be done **before** processing new channel entries, and the timeout could be
chosen to be coprime to the block interval (e.g., 79 seconds).

## Routing Logic

At this point, the peer set has information on recent inventory advertisements.
However, the `Service` trait only allows `poll_ready` to report readiness based
on the service's data and the type of the request, not the content of the
request.  This means that we must report readiness without knowing whether the
request should be routed to a specific peer, and we must handle the case where
`call` gets a request for an item only available at an unready peer.

This RFC suggests the following routing logic.  First, check whether the
request fetches data by hash.  If so, and `peers()` returns `Some(ref addrs)`,
iterate over `addrs` and route the request to the first ready peer if there is
one.  In all other cases, fall back to p2c routing.  Alternatives are suggested
and discussed below.

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

The rationale is described above.  The alternative choices are primarily around
the routing logic (to be filled in).

# Future possibilities
[future-possibilities]: #future-possibilities

(to be filled in)
