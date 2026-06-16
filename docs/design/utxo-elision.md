# Non-Survivor UTXO Elision in the Finalized State During Known-Hash Sync

- Status: **DESIGN ONLY — not implemented.** No code changes accompany this
  document. This is the crash-safety and write-path analysis that must be
  agreed before any implementation.
- Base: branch `ibd-engine` (the any-order write pipeline, `docs/design/known-hash-ibd.md` §7.3).
- Related prior attempt: an earlier elision experiment **crashed** mid-sync.
  The dominant purpose of this document is to explain *why* it crashed and to
  state the exact invariant under which elision is crash-safe, or to declare
  it unsafe.

## 0. TL;DR / verdict

Eliding non-survivor transparent outputs from the finalized state is **only
crash-safe under one specific, enforceable condition**:

> **Survivor-elision invariant (SEI):** at the moment a checkpoint block's
> spend is *resolved* (`check::utxo::transparent_spend`), the created UTXO
> being spent must still be resolvable from an *in-memory* source — the live
> non-finalized chain (`Chain::created_utxos`) or the recently-finalized
> `PrunedChain` cache — and must **never** need to fall through to
> `finalized_state.utxo(&spend)`.

If a created output is elided from `utxo_by_out_loc`, that DB fall-through
(`zebra-state/src/service/check/utxo.rs:179`) returns `None` →
`MissingTransparentOutput`, which the worker turns into a checkpoint
in-memory commit error and a queue reset (`write/worker.rs:329-363`). That is
the prior crash, restated precisely: **the spend resolution reads the created
output's value/script/`from_coinbase`/height, and if it was elided and has
also aged out of memory, resolution fails.**

The SEI is satisfiable, but it is **not** a free consequence of "survivors
are unspent at H_max". It requires the elided output's *spend* to be
processed while the output is still inside the in-memory window. The window
is `PrunedChain`'s retention (`checkpoint_sync_retained_blocks`, default =
`MAX_BLOCK_REORG_HEIGHT` = 1000 blocks). **A created-then-spent pair whose
gap exceeds the retention window CANNOT be safely elided** — even though it
nets to zero at H_max — because the spend would dereference an output that is
neither on disk (elided) nor in memory (aged out).

Therefore the safe design is **not** "elide every non-survivor". It is:

> **Elide an output's create iff it is a non-survivor at H_max AND its spend
> height is within the retention window of its create height** (so the
> `PrunedChain` cache still holds it at spend time). Outputs spent later than
> that are written normally and deleted normally.

This is a strict subset of "all non-survivors", but it is exactly the spam-era
create-then-spend population the `PrunedChain` cache was already built for
(`pruned_chain.rs` module docs: "most transparent outputs are spent within a
few hundred blocks of their creation"), so it captures the large majority of
the I/O win while remaining provably crash-safe.

The rest of this document grounds every claim in code and specifies the
write-path changes, the data structure, the threading, and the test plan.

---

## 1. The finalized write path for transparent outputs (as built)

### 1.1 Where the elision decision must land

The finalized transparent index is built by
`DiskWriteBatch::prepare_transparent_transaction_batch`
(`zebra-state/src/service/finalized_state/zebra_db/transparent.rs:445-507`),
which calls, in order:

1. `prepare_transparent_address_balance_updates` (in-memory balance math, no DB),
2. `prepare_new_transparent_outputs_batch` (`transparent.rs:613-666`) — inserts creates,
3. `prepare_spent_transparent_outputs_batch` (`transparent.rs:685-726`) — deletes spends,
4. `prepare_spending_transparent_tx_ids_batch` per tx (`transparent.rs:740-801`),
5. `prepare_transparent_balances_batch` (`transparent.rs:812-842`).

This whole function is invoked from
`DiskWriteBatch::prepare_block_batch` (`block.rs:643-710`), which is invoked
from `ZebraDb::write_block` (`block.rs:483-604`). `write_block` builds the
inputs:

- `new_outputs_by_out_loc: BTreeMap<OutputLocation, Utxo>` — derived from
  `finalized.new_outputs` (`block.rs:503-512`),
- `spent_utxos_by_out_loc: BTreeMap<OutputLocation, Utxo>` and
  `spent_utxos_by_outpoint: HashMap<OutPoint, Utxo>` — derived from the
  already-resolved `spent_utxos: Vec<(OutPoint, OutputLocation, Utxo)>` passed
  into `write_block` (`block.rs:514-529`).

The whole batch is committed atomically by `self.db.write(batch)`
(`block.rs:595-597`) — one RocksDB `WriteBatch` per block, so a block's
creates and deletes always land together or not at all.

**Decision: filter at batch-write time, inside
`prepare_new_transparent_outputs_batch` and
`prepare_spent_transparent_outputs_batch`.** Rationale:

- The `OutputLocation` is the survivor-set key, and it is computed exactly at
  this layer (`new_outputs_by_out_loc` keys; `spent_utxos_by_out_loc` keys).
  Filtering here needs no extra plumbing of locations.
- Filtering earlier (at commit-request construction in the engine or
  `convert.rs`) would require re-deriving `OutputLocation`s and would not see
  the spend side's `OutputLocation`s, which only exist after spend resolution.
- The address-index and balance passes (§3) read the same maps; filtering at
  this single layer lets us decide once and skip the dependent index writes
  consistently.

### 1.2 The create path (to be filtered)

`prepare_new_transparent_outputs_batch` (`transparent.rs:613-666`) loops
`for (new_output_location, utxo) in new_outputs_by_out_loc` and, per output,
writes up to three CFs:

- `utxo_loc_by_transparent_addr_loc` (address → unspent output index),
- `tx_loc_by_transparent_addr_loc` (address → tx index; **append-only,
  never deleted**, see the comment at `transparent.rs:653`),
- `utxo_by_out_loc` (the UTXO itself, `transparent.rs:664`).

### 1.3 The spend path (to be filtered in lockstep)

`prepare_spent_transparent_outputs_batch` (`transparent.rs:685-726`) loops
`for (spent_output_location, utxo) in spent_utxos_by_out_loc` and:

- deletes from `utxo_loc_by_transparent_addr_loc` (`transparent.rs:720`),
- deletes from `utxo_by_out_loc` (`transparent.rs:724`).

A `zs_delete` of a key that was never inserted (because we elided its create)
is a RocksDB no-op — harmless on its own — but the **balance** and
**received-total** passes are not no-ops (§3), so the spend must be elided in
exactly the same condition as its create, computed identically.

### 1.4 Critical separation: spend *resolution* vs. spend *write*

The value/script of a spent output is read during **resolution**, far
upstream of the batch:

- `check::utxo::transparent_spend` (`utxo.rs:41-102`) resolves each spend to a
  full `OrderedUtxo` via `transparent_spend_chain_order` (`utxo.rs:131-189`),
  whose fall-through order is: block's own new outputs → non-finalized
  unspent → `PrunedChain` cache (`utxo.rs:172-178`) → **`finalized_state.utxo(&spend)`**
  (`utxo.rs:179`) → `MissingTransparentOutput` (`utxo.rs:185`).
- The resolved `OrderedUtxo` carries value, script, height, and
  `from_coinbase`. `transparent_coinbase_spend` (`utxo.rs:84`, `:206-233`) reads
  `from_coinbase`/`height`; `remaining_transaction_value` (`utxo.rs:99`, `:247-296`)
  reads `output.value()`.

By the time the batch is built in `write_block`, the spent UTXO is already
fully in hand (the `spent_utxos` Vec argument). **The batch never re-reads the
created output from `utxo_by_out_loc` for value.** The only DB read of the
created output is the resolution fall-through at `utxo.rs:179`.

> This is the single load-bearing fact for crash safety: elision is safe iff
> resolution never reaches `utxo.rs:179` for an elided output. Everything in
> §2 is about proving that.

The value pool is likewise computed from the already-resolved
`utxos_spent_by_block` (`prepare_chain_value_pools_batch`, `chain.rs:249-301`,
via `chain_value_pool_change` at `chain.rs:256-261`), so eliding the
`utxo_by_out_loc` entry does **not** affect the value pool — provided the
spend was resolvable in memory.

---

## 2. Crash-safety, resume, and the intermediate-tip problem (the crux)

### 2.1 The hazard the prior attempt hit

`utxo_by_out_loc` is the live unspent set: creates insert, spends delete
(`transparent.rs:664`, `:724`); the emitter relies on exactly this
(`for_each_unspent_output_location_bytes`, `transparent.rs:199-211`). The
survivor set is defined at H_max: outputs unspent *at H_max*.

Two distinct heights matter for any output:

- `h_create` — the height that creates it,
- `h_spend` — the height that spends it (≤ H_max for a non-survivor).

The any-order write pipeline advances a **durable tip** `T` on disk
(`disk_tip_height`, published `Release`/`Acquire`, `disk_writer.rs:88-98`,
read by `prune_durable_blocks`, `worker.rs:928-947`). During sync,
`T < H_max`. The disk state at tip `T` is supposed to be a *consistent UTXO
set as of T* — that is what RPC, restart, and not-yet-synced spend lookups
assume.

Here is the trap. Consider an output with `h_create ≤ T < h_spend ≤ H_max`.
At H_max it is spent, so it is a non-survivor, so naive elision skips its
create. But at the current durable tip `T`, this output is **unspent and
should exist** in the UTXO set. We have removed it. The on-disk UTXO set at
`T` is now *missing a live output*. If anything reads the UTXO set at `T`
(a later block's spend resolution, RPC, a restart-and-resume), it sees a hole.

The prior crash is the in-sync manifestation: the engine commits `h_spend`'s
block; its spend resolves `transparent_spend` → falls through every in-memory
tier → `finalized_state.utxo(&spend)` at `utxo.rs:179` → `None` (we elided it)
→ `MissingTransparentOutput` → the worker's `commit_checkpoint_block` returns
`Err` (`worker.rs:329-363`) → queue reset → (on repeat) deterministic-failure
stop or a crash loop.

### 2.2 Is intermediate-tip consistency actually required?

We must answer this precisely, per reader:

**(a) Spend resolution for not-yet-synced blocks — YES, required, and this is
the killer.** This is an *internal* read of the intermediate UTXO set and it
is unavoidable during sync. `transparent_spend_chain_order` (`utxo.rs:131-189`)
consults the in-memory tiers first, then `finalized_state.utxo` last. For an
output created in a finalized (pruned-out) block, the *only* source of its
value is the DB — unless it is still in the `PrunedChain` cache. So
intermediate consistency is required **exactly to the extent that the
in-memory tiers don't already cover the spend**. This is what bounds elision
to the retention window (§2.4).

**(b) RPC at intermediate heights — divergent but not a crash.** `getrawtransaction`,
`gettxout`, `getaddressutxos`, `z_gettotalbalance` etc. would report an
incomplete UTXO set at intermediate heights for elided-but-not-yet-spent
outputs. During known-hash IBD the node is explicitly *not* at tip and RPC
UTXO/address queries are already understood to be incomplete (the node is
syncing). The known-hash design already accepts address-index divergence as
experiment-acceptable (§3). **However**, an elided output that is *also* a
not-yet-spent live output at tip `T` makes even `gettxout` for that specific
outpoint wrong, which is a stronger divergence than the address indexes.
Classify: **RPC-only, acceptable for the experiment, but only because the
node is mid-IBD; it must NOT persist past H_max** — and by construction it
doesn't, because every elided output is spent by H_max (its delete is also
elided, so the *final* UTXO set is identical; see §2.5).

**(c) Restart / resume — YES, required, and it constrains the design.** On
restart the engine re-derives `next_commit = best_tip_height() + 1`
(`known-hash-ibd.md` §4.7) and the `PrunedChain` cache is **rebuilt empty**
(it is created by `enable_pruned_chain` on the first checkpoint commit,
`worker.rs:319-323`; nothing repopulates it from disk). So after a restart at
durable tip `T`, the in-memory tiers are empty until new roots finalize. Any
block `h_spend > T` whose spent output had `h_create ≤ T` must resolve that
output from the **DB**. If we elided it, resolution fails immediately after
restart.

This is decisive: **the SEI must hold across restarts.** The retention-window
bound (§2.4) is *not by itself* restart-safe, because after a crash the cache
is cold. We need the stronger condition in §2.6.

### 2.3 What makes the *final* state correct (the easy half)

At H_max, an elided output has had **both** its create and its spend skipped.
Net effect on `utxo_by_out_loc`: zero (insert-then-delete elided = nothing).
Net effect on the value pool: the spend's value was still counted at
resolution time (the resolved `OrderedUtxo` flowed into
`chain_value_pool_change`), and the create's value was counted when the
creating block committed — so the chain value pool is unchanged. The
**survivor set on disk at H_max is byte-identical** to a normally-synced node,
because the survivor set is exactly the outputs whose create we did *not*
elide and whose spend never happened. This is the parity claim the test plan
(§5) must prove. The hard half is the *path* from genesis to H_max, not the
endpoint.

### 2.4 First containment: the retention window (necessary, not sufficient)

`PrunedChain` retains `checkpoint_sync_retained_blocks` (default 1000,
`pruned_chain.rs:36-50`, `:88-104`) and `add_finalized_root` caches a root's
still-unspent outputs as it is pruned (`non_finalized_state.rs:384-393`),
removing them on spend (`remove_spent`, called from
`commit_checkpoint_block`, `non_finalized_state.rs:600-602`). So for an output
with `h_spend - h_create ≤ retained_blocks`, the spend resolves from the cache
(`utxo.rs:172-178`) and never touches the DB — **even though we elided it.**

This makes in-sync elision safe for the common spam-era pattern. But it does
not survive a restart (§2.2c). So it is necessary but not sufficient.

### 2.5 Why naive "elide all non-survivors" is UNSAFE (explicit)

An output with `h_spend - h_create > retained_blocks` (a long-lived but
eventually-spent output) is a non-survivor at H_max, but:

- in-sync, its spend resolves *past* the cache window → `finalized_state.utxo`
  → `None` if elided → crash;
- across a restart, *any* elided output spent after `T` → `None` → crash.

So eliding all non-survivors is unsafe. The prior attempt almost certainly
elided on the survivor-set membership alone (unspent-at-H_max), which is
precisely this unsafe superset.

### 2.6 The crash-safe condition (sufficient, restart-proof)

Combine two requirements:

1. **In-sync:** the spend must resolve from memory ⇒ `h_spend - h_create ≤
   retained_blocks`.
2. **Restart-proof:** an elided output must never be needed from the DB after
   a restart. After a restart the durable tip is `T` and the cache is cold, so
   the dangerous outputs are those with `h_create ≤ T < h_spend`. To guarantee
   this never happens for an elided output, **the create and the spend must be
   in the same atomic durability step relative to `T`** — i.e. we must only
   elide an output once we *know* its spend has also been (or is being)
   committed, so that no durable tip `T` ever sits strictly between them.

The clean way to guarantee (2) is a **disk-frontier-gated elision**:

> **Elide an output's create iff its spend height `h_spend ≤ T_safe`**, where
> `T_safe` is a height such that the spending block is guaranteed to be
> committed in the same or an earlier durability epoch. The simplest
> sufficient choice that needs no new tip coordination is:
>
> **Only elide when `h_create` and `h_spend` are committed in the same
> "in-memory generation" — i.e. the output is created and spent without the
> creating block's root ever becoming durable while the spend is still
> pending.**

In practice this is *implied by* the retention-window bound **plus** a
restart rule: on restart, **disable elision until the engine has re-passed
H_max-relative safety** — but that is fragile. The robust, simple rule is:

> **Decision (final): elide iff `survivor_set` says the output is a
> non-survivor AND `h_spend - h_create ≤ retained_blocks` AND we additionally
> require that the create is not yet durable when we decide.** Because the
> write pipeline commits in height order and the create is only durable once
> its block is written, and `h_spend > h_create`, the create's block is
> written *before* the spend's block. To keep the intermediate disk state
> from ever exposing a hole that a restart could trip on, we **defer the
> create write decision to the spend's block** is *not possible* (creates and
> spends are in different blocks/batches, each atomic).

This forces the real design choice, stated honestly below.

### 2.7 The honest design choice

There are exactly two ways to make elision restart-safe; pick (A):

**(A) Window-bounded elision + cold-cache safety net (RECOMMENDED).**
Elide only when `h_spend - h_create ≤ retained_blocks`. Accept that across a
restart the cache is cold, and close that gap with a **bounded fallback**:
when `transparent_spend_chain_order` would return `MissingTransparentOutput`
*during known-hash IBD only*, consult an auxiliary on-disk structure that
records, for each elided outpoint, the minimal data the spend needs (value,
script, height, from_coinbase) until its spend height passes the durable tip.
This is, in effect, a **"pending-elided-outputs" side table** that lives only
between an elided create and its spend, and is itself crash-safe because it is
written in the *same atomic batch* as the elided create's block.

But a side table that stores value+script+height+from_coinbase per elided
output **is just `utxo_by_out_loc` under another name** — it defeats the
purpose. So (A) collapses to: *don't elide; or elide only the address
indexes, not the UTXO itself.* That is a real, smaller win (§3.5) and it is
unconditionally crash-safe.

**(B) Two-pass / deferred-durability elision (the only way to elide the UTXO
itself safely).** Make the *create* write conditional on the *spend* being
durable, by **holding back the create from disk until its spend's block is
also ready to commit**, then writing neither. Concretely: the disk writer,
instead of writing each block's `utxo_by_out_loc` inserts immediately, would
buffer inserts for outputs the survivor set marks as "non-survivor, spent
within the window" and drop them when the spending block arrives in the same
buffered span. A crash flushes the buffer as **normal (non-elided) writes**,
so the disk is always either "both present" or "both absent" — never a hole.

This is implementable but it adds a durability-buffer to the disk writer and
changes the atomicity story (a block's UTXO inserts may now be split across
batches). It is the only approach that elides the UTXO bytes *and* is
restart-safe. It is more invasive and is the subject of §4.4.

> **Recommendation for a first, provably-safe experiment: do NOT elide the
> `utxo_by_out_loc` bytes. Implement the unconditionally-safe address-index
> elision of §3.5 first** (no crash surface at all, since spend resolution
> never touches the address indexes). Then, if the UTXO-bytes win is required,
> implement (B) behind a config flag with the §5 parity tests as the gate.

---

## 3. Address-index divergence analysis

For each CF touched by the create/spend passes, classify what diverges from a
normally-synced node if we elide a non-survivor, and whether it is
consensus-critical or RPC-index-only.

| CF | Written by | If we elide the pair (create+spend) | Class |
|---|---|---|---|
| `utxo_by_out_loc` | create insert `transparent.rs:664`; spend delete `transparent.rs:724` | Net-zero at H_max (insert+delete both skipped). **Identical** final survivor set. *Intermediate* state has a hole (§2). | **Consensus-relevant via spend resolution** (`utxo.rs:179`): must be in-memory-resolvable or it crashes. The *final* state is correct. |
| `utxo_loc_by_transparent_addr_loc` | create insert `transparent.rs:646`; spend delete `transparent.rs:720` | Net-zero at H_max. Intermediate hole only matters to `address_utxos`/`address_utxo_locations` (`transparent.rs:215-280`). | **RPC-index-only.** Never read by consensus or spend resolution. |
| `tx_loc_by_transparent_addr_loc` | create insert `transparent.rs:658`; spend's spending-tx insert `transparent.rs:786` | **Append-only — never deleted.** Eliding the create's entry **permanently loses** the "address received in this tx" record; the spending-tx entry (`transparent.rs:786`) is a *different* key (the spend path) and is unaffected by create-elision. | **RPC-index-only, but DIVERGES PERMANENTLY** from a normal node (the address tx history is missing the elided receive). Acceptable only as an explicit experiment caveat. |
| `balance_by_transparent_addr` (balance) | merge/insert from `address_balances` (`transparent.rs:812-842`); credit on create, debit on spend (`transparent.rs:523-593`) | If we skip BOTH the credit and the debit for the elided pair, the balance is net-zero-correct at H_max **and at every intermediate height** (credit and debit are equal and opposite). | **Must be kept self-consistent:** elide the create-credit and the spend-debit *together*. If kept together, the final balance is **identical**. |
| `balance_by_transparent_addr` (received total, the `u64`) | `received` accumulates on every credit, **never decremented** (`AddressBalanceLocationInner::receive_output`) | Eliding the create's credit **lowers the received total permanently**. A normal node counts the receive even though it was later spent. | **RPC-index-only, DIVERGES PERMANENTLY.** `address_balance().1` (received) will be lower than a normal node. Acceptable experiment caveat; **not** consensus. |
| `tx_loc_by_spent_out_loc` (indexer feature only) | spend insert `transparent.rs:795-798` | Keyed by the spent output location; if we elide we'd skip it. Indexer-only. | **RPC/indexer-only.** Behind `feature = "indexer"`. |
| `chain_value_pools` / `block_info` | `prepare_chain_value_pools_batch` (`chain.rs:249-301`) | Computed from resolved spends, **not** from `utxo_by_out_loc`. Unaffected by elision *as long as the spend resolved*. | **Consensus-critical, but independent of elision** (depends only on resolution succeeding). |

### 3.5 The unconditionally-safe subset: address-index elision

The address indexes (`utxo_loc_by_transparent_addr_loc`, the create side of
`tx_loc_by_transparent_addr_loc`, and the balance credit+debit pair) are
**never read by consensus, spend resolution, the value pool, or the engine.**
They are pure RPC indexes. Eliding the *address-index writes* for
non-survivor outputs (while still writing `utxo_by_out_loc` normally) is
therefore **crash-safe with zero risk to the sync path**: a crash and resume
finds a fully consistent UTXO set; only the RPC address indexes are sparser.

Wins from this subset:
- skips up to 2 inserts + 1 delete per non-survivor output in the address CFs,
- skips the balance credit/debit churn for net-zero addresses,
- in the spam era (the densest blocks), this is the bulk of the address-index
  write volume.

Caveats (must be documented as experiment behavior):
- `getaddressbalance` received-total and `getaddresstxids` will diverge
  permanently for elided receives (the append-only `tx_loc_by_*` and the
  monotone received counter).
- `getaddressbalance` *balance* and `getaddressutxos` remain correct at H_max
  (net-zero) but are sparse at intermediate heights.

This is the recommended Phase 1 (§5).

---

## 4. Write-path code changes (sketches with anchors)

All sketches assume an optional survivor-set handle is threaded to the batch
layer (threading in §4.3). The handle answers one query:

```rust
/// Is the output at `loc` a survivor (unspent at H_max)?
/// `None` everywhere ⇒ no elision (normal sync).
fn is_survivor(&self, loc: &OutputLocation) -> bool;
```

### 4.1 Create-side filter (address indexes only — Phase 1, safe)

In `prepare_new_transparent_outputs_batch`
(`transparent.rs:613-666`), thread `survivors: Option<&SurvivorSet>` and gate
the **address-index** writes only (NOT the `utxo_by_out_loc` write in Phase 1):

```rust
for (new_output_location, utxo) in new_outputs_by_out_loc {
    let unspent_output = &utxo.output;
    let receiving_address = unspent_output.address(network);

    // Phase 1: skip the RPC address indexes for non-survivors; ALWAYS write
    // utxo_by_out_loc so spend resolution and restart stay correct.
    let elide_addr_index = survivors
        .map(|s| !s.is_survivor(new_output_location))
        .unwrap_or(false);

    if let Some(receiving_address) = receiving_address {
        if !elide_addr_index {
            // ... existing utxo_loc_by_transparent_addr_loc insert (transparent.rs:646)
            // ... existing tx_loc_by_transparent_addr_loc insert  (transparent.rs:658)
        }
    }

    // Phase 1: unconditional. Phase 2 (§4.4): conditional on (B).
    self.zs_insert(&utxo_by_out_loc, new_output_location, unspent_output);
}
```

The balance pass must be elided **in the same condition** to stay net-zero
consistent. Because balances are computed in
`prepare_transparent_address_balance_updates` (`transparent.rs:523-593`)
*before* the output passes, the cleanest hook is to **skip both the
input-debit and the output-credit** there for elided pairs. That requires the
survivor query at the balance layer too; pass `survivors` into
`prepare_transparent_transaction_batch` (`transparent.rs:445`) and down.

> Subtlety: a debit and its matching credit are in *different blocks*
> (`h_create` ≠ `h_spend`). To keep every per-block batch self-consistent,
> the simplest correct rule is: **skip the credit at create iff the output is
> elidable, and skip the debit at spend iff the spent output was elided.**
> Both decisions use the same `is_survivor(loc)` test on the same
> `OutputLocation`, so they agree by construction. The intermediate per-block
> balance stays correct because each block skips exactly the credits/debits of
> outputs that are invisible to that block's address index.

### 4.2 Spend-side filter (address indexes only — Phase 1, safe)

In `prepare_spent_transparent_outputs_batch` (`transparent.rs:685-726`), gate
the address-index delete (and, Phase 2 only, the `utxo_by_out_loc` delete) on
the same survivor test applied to the **spent** output's location:

```rust
for (spent_output_location, utxo) in spent_utxos_by_out_loc {
    let elided = survivors
        .map(|s| !s.is_survivor(spent_output_location))
        .unwrap_or(false);

    if let Some(sending_address) = utxo.output.address(network) {
        if !elided {
            // ... existing utxo_loc_by_transparent_addr_loc delete (transparent.rs:720)
        }
    }

    // Phase 1: unconditional delete (the create was written, so delete it).
    // Phase 2 (B): if `elided`, the create was never written → skip the
    // delete (a no-op anyway, but skip the address-index work).
    self.zs_delete(&utxo_by_out_loc, spent_output_location);
}
```

Because `is_survivor` is a pure function of `OutputLocation`, the create-side
and spend-side decisions are guaranteed to agree on the same output — the
core consistency property.

### 4.3 Threading the survivor-set handle

`FinalizedState` and `ZebraDb` are constructed in `FinalizedState::new` /
`new_with_debug` (`finalized_state.rs:151`+) from `&Config`. The survivor set
is an optional, read-only artifact loaded once at startup (like the
known-hash list). Plumbing:

1. Add to `zebra_state::Config` (`config.rs:26`) an optional path:
   `pub survivor_set_path: Option<PathBuf>` (serde `default`, documented).
2. In `FinalizedState::new_with_debug`, if the path is set and the DB is empty
   (fresh sync only — never load survivor elision against an existing
   non-empty DB, which could already hold the now-"non-survivor" outputs),
   memory-map/load it into an `Arc<SurvivorSet>` stored on `ZebraDb`.
3. `ZebraDb` exposes `survivor_set(&self) -> Option<&SurvivorSet>`; the disk
   writer's `write_block` reads it and passes `Option<&SurvivorSet>` down
   through `prepare_block_batch` → `prepare_transparent_transaction_batch` →
   the create/spend/balance passes.

The disk writer (`disk_writer.rs`) already owns the only `commit_finalized_direct`
path, so the handle reaches exactly one place. No change to the worker, the
engine, or the network is needed.

Guards (must be enforced; otherwise corruption):
- **Fresh DB only.** Refuse to enable elision if `db.finalized_tip_height()`
  is `Some` and above genesis, *unless* the durable tip is below the survivor
  set's H_max and we can prove resume-safety (Phase 2 only). Phase 1
  (address-index) is safe to resume because it never elides `utxo_by_out_loc`.
- **H_max alignment.** The survivor set's H_max must equal the engine's
  `list.max_height()` (or be ≥ the sync target). A mismatched survivor set
  would mark wrong outputs; refuse to load on mismatch (store H_max in the
  artifact header).
- **Network match.** Survivor sets are per-network; embed a network tag.

### 4.4 Phase 2: eliding the UTXO bytes safely (approach B)

To elide `utxo_by_out_loc` itself and stay restart-safe, the disk writer must
guarantee the disk never shows a "non-survivor present at create, absent at
spend" hole across a crash. Mechanism:

- The disk writer keeps a small **pending-elision buffer**: when a block's
  batch is built, instead of skipping a non-survivor create outright, it
  records `(OutputLocation, h_spend_expected)` and **withholds only the
  `utxo_by_out_loc` insert** from this block's batch, writing everything else.
- When the spending block (height `h_spend`) is committed, the buffer drops
  the entry — the insert was never written and the delete is skipped: net
  zero, and **no durable tip ever sat between a present create and its
  delete**, because the create was never made durable.
- On a crash: anything still in the buffer was *not* written, so the disk has
  neither the create nor the spend's effects for those outputs — but the
  spending block also hasn't committed (it is `> T`), so the disk is
  consistent as of `T`. On restart, the engine re-fetches from `T+1`; those
  creates are re-processed and re-buffered (or written normally if the new
  survivor decision differs). **No hole is ever durable.**

Constraints this imposes:
- The buffer must be bounded (by the same retention window: an entry whose
  `h_spend` exceeds `T + retained_blocks` must be **flushed as a normal
  write**, because we can no longer guarantee the spend lands soon — this is
  the §2.4 window made durable).
- A block's `utxo_by_out_loc` writes are now potentially split across the
  block's own batch and a later flush, so the per-block atomic-batch property
  is weakened to "per-block batch ∪ buffer is consistent as of the durable
  tip". The buffer is in-memory only and reconstructed from re-sync on crash,
  so this is acceptable, but it must be documented and tested (§5).

Phase 2 is **optional** and gated behind a config flag, defaulting off. Phase
1 is the recommended deliverable.

---

## 5. Survivor-set data structure

The artifact is the emitter's output: sorted 8-byte big-endian
`OutputLocation`s of every output unspent at H_max
(`emit_snapshot.rs:94-117`, `for_each_unspent_output_location_bytes`,
`transparent.rs:199-211`). `OutputLocation` is 8 bytes
(`OUTPUT_LOCATION_DISK_BYTES`, = 3-byte height + 2-byte tx index + 3-byte
output index, all big-endian, `transparent.rs:52`, `:726-738`), so **byte
order equals location order** — the file is sorted both as bytes and as
locations. Mainnet has tens of millions of entries (testnet ~10M+).

The query is a membership test on `OutputLocation` (the same value the create
and spend passes already hold).

### 5.1 Recommendation: memory-mapped sorted slice + binary search

- `mmap` the file read-only; reinterpret as `&[[u8; 8]]` (the file is exactly
  `8 × N` bytes; reject if not). No parsing, no heap, page-cache-backed.
- `is_survivor(loc)` = `slice.binary_search(&loc.as_bytes())` is `Ok`.
- Memory: ~0 RSS beyond touched pages (the OS pages it in on demand and
  evicts under pressure). For 50M entries that is a 400 MB file, but resident
  set tracks the working set, not the whole file.
- Latency: `log2(50M) ≈ 26` comparisons, each an 8-byte memcmp — a few
  hundred ns worst case, mostly L2/L3 after warmup because the engine queries
  in ascending height order (the binary search's upper levels stay hot, and
  the lower levels walk forward with locality). The query is per-created-output,
  off the disk-writer thread's hot loop, so even a cold-page fault
  (~10 µs) is dwarfed by the RocksDB write it is gating.

Why not the alternatives:
- **Sorted `Vec<u64>` in RAM:** 400 MB resident, defeating the memory win of
  not writing the outputs; only marginally faster than mmap after warmup.
- **RoaringBitmap:** `OutputLocation` is not a dense `u32`/`u64` index space
  (height-major, sparse output indexes); building a roaring set over a 48-bit
  key space loses the locality and adds a dependency. The sorted file already
  *is* the compressed form for this access pattern.
- **Bloom filter + fallback:** a false positive means "treat a survivor as
  elidable" → we'd skip a create that should persist → **corrupts the final
  UTXO set**. A Bloom filter's error direction is exactly wrong here unless
  paired with an exact fallback, which is the sorted slice anyway. Reject.

### 5.2 Loading

Load once at `FinalizedState::new_with_debug` on a blocking context (it is a
syscall `mmap`, not a read of the whole file). Validate: file size `% 8 == 0`,
strictly ascending (spot-check first/last + a sample, or trust the emitter and
assert in tests), header tag for network + H_max (store these in a tiny
sidecar or a fixed-size prefix, since the raw `.bin` has none today — add a
small header to the emitter, or a companion `.meta` file). Wrap in `Arc`,
store on `ZebraDb`.

---

## 6. Phased, testable implementation plan

### Phase 0 — Emitter metadata (small, enabling)
- Add a header/sidecar to the emitted artifact recording network + H_max +
  entry count (`emit_snapshot.rs`), so the loader can refuse a mismatched set.
- Test: round-trip the header; reject wrong network / wrong H_max.

### Phase 1 — Address-index elision only (RECOMMENDED FIRST, unconditionally crash-safe)
- `SurvivorSet` (mmap + binary search, §5).
- Config field + `ZebraDb` handle + threading (§4.3).
- Gate the address-index writes (create + spend) and the balance credit/debit
  pair on `is_survivor` (§4.1, §4.2), **never** touching `utxo_by_out_loc`.
- Tests:
  - **Spend-resolution safety:** a sync with elision enabled never produces
    `MissingTransparentOutput` (assert the metric / error count is zero). This
    is the direct anti-regression for the prior crash, and it passes trivially
    in Phase 1 because `utxo_by_out_loc` is untouched — which is the point.
  - **Crash/resume:** kill the process mid-sync, restart, finish; assert no
    spend-resolution failures and a correct tip.
  - **Balance net-zero:** unit test on `prepare_transparent_transaction_batch`
    with a hand-built create+spend pair for one address: balance after the
    pair equals balance with elision off.

### Phase 2 — UTXO-byte elision via the deferred-durability buffer (OPTIONAL, flagged off)
- Implement approach B (§4.4) in the disk writer: pending-elision buffer,
  window-bounded flush-as-normal, drop-on-spend.
- Tests:
  - **No durable hole:** a property/integration test that, after committing up
    to every intermediate height `T`, opens the DB read-only and asserts the
    UTXO set at `T` is exactly the set a normally-synced node has at `T` for
    all outpoints whose spend height `> T` (i.e. no live output is missing).
  - **Crash mid-buffer:** crash with entries in the buffer; restart; assert
    the resumed sync produces the identical H_max state and zero spend
    failures.

### Phase 3 — H_max parity gate (the master correctness test, both phases)
- **UTXO-set-at-H_max parity:** sync a small network (regtest fixture or a
  short testnet range) twice — once normally, once with elision — to the same
  H_max, then byte-compare the live `utxo_by_out_loc` set via
  `for_each_unspent_output_location_bytes` (`transparent.rs:199-211`). They
  MUST be identical. This is the single test that proves elision preserves the
  final UTXO set; it is the gate for both phases.
- **Value-pool parity:** assert `chain_value_pools` and per-`block_info`
  pools are identical at H_max.
- **Tip-hash parity:** assert identical finalized tip hash (already a
  known-hash-ibd validation gate, §11).

### Test fixtures
- A deterministic regtest/short-testnet chain containing: (a) a
  create-then-spend pair within the retention window, (b) a pair spanning the
  window boundary (must NOT be elided — Phase 2), (c) a survivor created near
  H_max and never spent (must be written), (d) a coinbase output spent after
  maturity (exercises `from_coinbase`/maturity resolution under elision).
- Reuse `zebra-test` vectors and the existing finalized-state batch tests
  (`zebra-state/src/service/finalized_state/zebra_db/transparent.rs` test
  modules and `pruned_chain.rs` tests as patterns).

---

## 7. Summary of the skeptical conclusion

- **The prior crash is fully explained:** spend resolution
  (`utxo.rs:131-189`) reads the created output's value, and its last resort is
  `finalized_state.utxo` (`utxo.rs:179`). Eliding a non-survivor whose spend
  is processed after it has left every in-memory tier turns that read into
  `None` → `MissingTransparentOutput` → checkpoint commit error → reset/crash.
- **Naive "elide all non-survivors" is unsafe** (long-gap pairs, and *any*
  pair across a restart with a cold cache).
- **Eliding the address indexes only is unconditionally safe** and captures
  most of the spam-era write volume; ship this first.
- **Eliding the `utxo_by_out_loc` bytes safely requires a deferred-durability
  buffer** (approach B) so the disk never durably shows a non-survivor between
  its create and its spend; this is the only restart-proof way and is the
  optional, flagged Phase 2.
- The **final** H_max state is provably identical to a normal sync in both
  phases (insert+delete net to zero, value pool counted at resolution); the
  hardness is entirely in the **intermediate** and **resume** states, which is
  why the buffer/window machinery — not the survivor set itself — is the real
  design.
