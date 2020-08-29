- Start Date: 2020-08-10
- Design PR: [ZcashFoundation/zebra#868](https://github.com/ZcashFoundation/zebra/pull/868)
- Zebra Issue: [ZcashFoundation/zebra#964](https://github.com/ZcashFoundation/zebra/issues/964)

# Summary
[summary]: #summary

This RFC describes an architecture for asynchronous script verification and
its interaction with the state layer. This architecture imposes constraints
on the ordering of operations in the state layer.

# Motivation
[motivation]: #motivation

As in the rest of Zebra, we want to express our work as a collection of
work-items with explicit dependencies, then execute these items concurrently
and in parallel on a thread pool.  

# Definitions
[definitions]: #definitions

- *UTXO*: unspent transaction output. Transaction outputs are modeled in `zebra-chain` by the [`TransparentOutput`][transout] structure.
- Transaction input: an output of a previous transaction consumed by a later transaction (the one it is an input to).  Modeled in `zebra-chain` by the [`TransparentInput`][transin] structure.
- lock script: the script that defines the conditions under which some UTXO can be spent.  Stored in the [`TransparentOutput::lock_script`][lock_script] field.
- unlock script: a script satisfying the conditions of the lock script, allowing a UTXO to be spent.  Stored in the [`TransparentInput::PrevOut::lock_script`][lock_script] field.

[transout]: https://doc.zebra.zfnd.org/zebra_chain/transaction/struct.TransparentOutput.html
[lock_script]: https://doc.zebra.zfnd.org/zebra_chain/transaction/struct.TransparentOutput.html#structfield.lock_script
[transin]: https://doc.zebra.zfnd.org/zebra_chain/transaction/enum.TransparentInput.html
[unlock_script]: https://doc.zebra.zfnd.org/zebra_chain/transaction/enum.TransparentInput.html#variant.PrevOut.field.unlock_script

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

Zcash's transparent address system is inherited from Bitcoin. Transactions
spend unspent transaction outputs (UTXOs) from previous transactions. These
UTXOs are encumbered by *locking scripts* that define the conditions under
which they can be spent, e.g., requiring a signature from a certain key.
Transactions wishing to spend UTXOs supply an *unlocking script* that should
satisfy the conditions of the locking script for each input they wish to
spend.

This means that script verification requires access to data about previous
UTXOs, in order to determine the conditions under which those UTXOs can be
spent. In Zebra, we aim to run operations asychronously and out-of-order to
the greatest extent possible. For instance, we may begin verification of a
block before all of its ancestors have been verified or even downloaded. So
we need to design a mechanism that allows script verification to declare its
data dependencies and execute as soon as all required data is available.

It's not necessary for this mechanism to ensure that the transaction outputs
remain unspent, only to give enough information to perform script
verification. Checking that all transaction inputs are actually unspent is
done later, at the point that its containing block is committed to the chain.

At a high level, this adds a new request/response pair to the state service:

- `Request::AwaitUtxo(OutPoint)` requests a `TransparentOutput` specified by `OutPoint` from the state layer;
- `Response::Utxo(TransparentOutput)` supplies requested the `TransparentOutput`.

Note that this request is named differently from the other requests,
`AwaitUtxo` rather than `GetUtxo` or similar. This is because the request has
rather different behavior: the request does not complete until the state
service learns about a UTXO matching the request, which could be never. For
instance, if the transaction output was already spent, the service is not
required to return a response. The caller is responsible for using a timeout
layer or some other mechanism.

This allows a script verifier to asynchronously obtain information about
previous transaction outputs and start verifying scripts as soon as the data
is available. For instance, if we begin parallel download and verification of
500 blocks, we should be able to begin script verification of all scripts
referencing outputs from existing blocks in parallel, and begin verification
of scripts referencing outputs from new blocks as soon as they are committed
to the chain.  

Because spending outputs from older blocks is more common than spending
outputs from recent blocks, this should allow a significant amount of
parallelism.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

We add a `Request::AwaitUtxo(OutPoint)` and
`Response::Utxo(TransparentOutput)` to the state protocol. As described
above, the request name is intended to indicate the request's behavior: the
request does not resolve until the state layer learns of a UTXO described by
the request.

To verify scripts, a script verifier requests the relevant UTXOs from the
state service and waits for all of them to resolve, or fails verification
with a timeout error. Currently, we outsource script verification to
`zcash_consensus`, which does FFI into the same C++ code as `zcashd` uses.
**We need to ensure this code is thread-safe**.

Implementing the state request correctly requires considering two sets of behaviors:

1. behaviors related to the state's external API (a `Buffer`ed `tower::Service`);
2. behaviors related to the state's internal implementation (using `sled`).

Making this distinction helps us to ensure we don't accidentally leak
"internal" behaviors into "external" behaviors, which would violate
encapsulation and make it more difficult to replace `sled`.

In the first category, our state is presented to the rest of the application
as a `Buffer`ed `tower::Service`. The `Buffer` wrapper allows shared access
to a service using an actor model, moving the service to be shared into a
worker task and passing messages to it over an multi-producer single-consumer
(mpsc) channel. The worker task receives messages and makes `Service::call`s.
The `Service::call` method returns a `Future`, and the service is allowed to
decide how much work it wants to do synchronously (in `call`) and how much
work it wants to do asynchronously (in the `Future` it returns).

This means that our external API ensures that the state service sees a
linearized sequence of state requests, although the exact ordering is
unpredictable when there are multiple senders making requests.

In the second category, the Sled API presents itself synchronously, but
database and tree handles are clonable and can be moved between threads. All
that's required to process some request asynchronously is to clone the
appropriate handle, move it into an async block, and make the call as part of
the future. (We might want to use Tokio's blocking API for this, but that's a
side detail).

Because the state service has exclusive access to the sled database, and the
state service sees a linearized sequence of state requests, we have an easy
way to opt in to asynchronous database access. We can perform sled operations
synchronously in the `Service::call`, waiting for them to complete, and be
sure that all future requests will see the resulting sled state. Or, we can
perform sled operations asynchronously in the future returned by
`Service::call`.

If we perform all *writes* synchronously and allow reads to be either
synchronous or asynchronous, we ensure that writes cannot race each other.
Asynchronous reads are guaranteed to read at least the state present at the
time the request was processed, or a later state.

Now, returning to the UTXO lookup problem, we can map out the possible states
with this restriction in mind. This description assumes that UTXO storage is
split into disjoint sets, one in-memory (e.g., blocks after the reorg limit)
and the other in sled (e.g., blocks after the reorg limit). The details of
this storage are not important for this design, only that the two sets are
disjoint.

When the state service processes a `Request::AwaitUtxo(OutPoint)` referencing
some UTXO `u`, there are three disjoint possibilities:

1. `u` is already contained in an in-memory block storage;
2. `u` is already contained in the sled UTXO set;
3. `u` is not yet known to the state service.

In case 3, we need to queue `u` and scan all *future* blocks to see whether
they contain `u`. However, if we have a mechanism to queue `u`, we can
perform check 2 asynchronously, because restricting to synchronous writes
means that any async read will return the current or later state. If `u` was
in the sled UTXO set when the request was processed, the only way that an
async read would not return `u` is if the UTXO were spent, in which case the
service is not required to return a response.

This behavior can be encapsulated into a `PendingUtxos`
structure described below.

```rust
// sketch
#[derive(Default, Debug)]
struct PendingUtxos(HashMap<OutPoint, oneshot::Sender<TransparentOutput>>);

impl PendingUtxos {
    // adds the outpoint and returns (wrapped) rx end of oneshot
    // return can be converted to `Service::Future`
    pub fn queue(&mut self, outpoint: OutPoint) -> impl Future<Output=Result<Response, ...>>;

    // if outpoint is a hashmap key, remove the entry and send output on the channel
    pub fn respond(&mut self, outpoint: OutPoint, output: TransparentOutput);


    // scans the hashmap and removes any entries with closed senders
    pub fn prune(&mut self);
}
```

The state service should maintain an `Arc<Mutex<PendingUtxos>>`, used as follows:

1. In `Service::call(Request::AwaitUtxo(u))`, the service should:
  - call `PendingUtxos::queue(u)` to get a future `f` to return to the caller;
  spawn a task that does a sled lookup for `u`, calling `PendingUtxos::respond(u, output)` if present;
  - check the in-memory storage for `u`, calling `PendingUtxos::respond(u, output)` if present;
  - return `f` to the caller (it may already be ready).
  The common case is that `u` references an old UTXO, so spawning the lookup
  task first means that we don't wait to check in-memory storage for `u`
  before starting the sled lookup.

2. In `Service::call(Request::CommitBlock(block, ..))`, the service should:
  - call `PendingUtxos::check_block(block.as_ref())`;
  - do any other transactional checks before committing a block as normal.
  Because the `AwaitUtxo` request is informational, there's no need to do
  the transactional checks before matching against pending UTXO requests,
  and doing so upfront potentially notifies other tasks earlier.

3. In `Service::poll_ready()`, the service should call
  `PendingUtxos::prune()` at least *some* of the time. This is required because
  when a consumer uses a timeout layer, the cancelled requests should be
  flushed from the queue to avoid a resource leak. However, doing this on every
  call will result in us spending a bunch of time iterating over the hashmap.

# Drawbacks
[drawbacks]: #drawbacks

One drawback of this design is that we may have to wait on a lock. However,
the critical section basically amounts to a hash lookup and a channel send,
so I don't think that we're likely to run into problems with long contended
periods, and it's unlikely that we would get a deadlock.

# Rationale and alternatives
[rationale-and-alternatives]: #rationale-and-alternatives

High-level design rationale is inline with the design sketch. One low-level
option would be to avoid encapsulating behavior in the `PendingUtxos` and
just have an `Arc<Hashmap<..>>`, so that the lock only protects the hashmap
lookup and not sending through the channel. But I think the current design is
cleaner and the cost is probably not too large.

# Unresolved questions
[unresolved-questions]: #unresolved-questions

- We need to pick a timeout for UTXO lookup. This should be long enough to
account for the fact that we may start verifying blocks before all of their
ancestors are downloaded.
