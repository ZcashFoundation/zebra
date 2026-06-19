# P2P Optimization Handoff

Last updated: 2026-06-19 06:03 America/Chicago.

## Goal

Make Zakura block sync execution-bound for a full from-scratch mainnet sync:

- analyzer blocking breakdown should be approximately 100% `commit_backpressure`
- `hol_gap`, `peer_unavailable`, `header_limited`, and `download_starved` should be approximately 0%
- the 4 GiB block download byte budget should stay near full
- connection churn should stay at zero or near-zero for the whole run

The authoritative objective is still:
`/home/evan/.codex/attachments/e174cb77-bcba-4bf0-aeb5-a3ff6f91f673/pasted-text-1.txt`.

## Coordination Guardrail

Another agent is actively working in this repo. The main worktree is dirty across
block-sync, state, and sync files. Do not revert, reset, overwrite, or clean up
dirty files you did not change. Keep experiments isolated unless a patch is
proven and intentionally ported.

Use `/tmp/zebra-zakura-slottrace` for isolated P2P experiments. The shared repo
currently has only documentation updates from this handoff/runbook work.

## Current Best Experiment

Best measured patch remains:

- commit: `764ceb050c51d597ce7678c98bcd6572d285c7cb`
- run: `/home/evan/src/valar/art/zakura-p2p-runs/run-764ceb05-20260619T101426Z`
- result:
  - `commit_backpressure`: 98.48%
  - `hol_gap`: 0.000s
  - downloaded unique: 109,038 blocks
  - finalized: 106,604 blocks
  - downloaded unique rate: 106.900 blk/s
  - finalized rate: 104.514 blk/s
  - reserved budget: 5.04%
  - conn rows: 14
  - `cancelled` closes: 0
  - block-sync misbehavior actions: 0

This is still not the goal because budget utilization is far too low, but it is
the current best baseline.

## What We Just Did

We tested whether stale hard timeout retry exclusions were causing
`no_assignable_range` and low budget fill.

Isolated experiment commit:

- commit: `53c6c502f27393d7c28698adf0bad81c4ee05392`
- branch/worktree: `/tmp/zebra-zakura-slottrace`
- change:
  - clear scheduler retry exclusions on body-download pause returns
  - clear scheduler retry exclusions after each scheduler pass
  - add `scheduler_timeout_exclusion_can_be_cleared_after_one_pass`

Focused checks passed:

- `cargo fmt -p zebra-network --check`
- `cargo test -p zebra-network scheduler_timeout`
- `cargo test -p zebra-network reactor_timeout_retry_uses_a_different_peer`
- `cargo test -p zebra-network reactor_fill_loop_saturates_multiple_slots_in_one_pass`
- `cargo test -p zebra-network status`
- `cargo check -p zebra-network`

Built and deployed binary:

- binary SHA: `077b23877bc5f60dda864c309765d9980202255574c1162555266527a8354488`
- target: `asia-pacific-0` / `168.144.173.250`
- all eight serving peers were also refreshed with the same binary, preserving
  their state:
  - `139.59.64.115`
  - `159.203.38.10`
  - `161.35.156.226`
  - `64.227.44.93`
  - `159.65.183.89`
  - `143.244.184.176`
  - `165.22.54.66`
  - `104.131.184.123`

After refreshing serving peers, the measurement target was reset from scratch
again so the trace window did not include peer restart churn.

Collected run:

- run: `/home/evan/src/valar/art/zakura-p2p-runs/run-53c6c502-20260619T110012Z`
- trace dir: `/home/evan/src/valar/art/zakura-p2p-runs/run-53c6c502-20260619T110012Z/zakura`
- analyzer output: `/home/evan/src/valar/art/zakura-p2p-runs/run-53c6c502-20260619T110012Z/analysis/summary.md`
- end sample before collection: 76,496 blocks / 76,496 headers
- target was stopped during collection, so traces are stable

Result: reject `53c6c502`.

Compared to `764ceb050`:

- downloaded blocks/s: 106.900 -> 77.687
- finalized blocks/s: 104.514 -> 75.116
- reserved budget: 5.04% -> 1.05%
- `commit_backpressure`: 98.48% -> 90.97%
- `hol_gap`: 0.000s -> 74.432s / 7.52%
- conn rows: 14 -> 53
- `cancelled` closes: still 0

Important diagnostic:

- all 28,672 schedule skips were still `no_assignable_range`
- no block-sync misbehavior actions
- HoL diagnostics showed the floor was servable by 8 peers, but available peers
  were 0 during gaps

Conclusion: clearing retry exclusions after each scheduler pass did not solve
budget fill and regressed HoL/throughput.

## Current External State

The measurement target `asia-pacific-0` was stopped by collection with
`--stop-first`.

The eight serving peers were left running `53c6c502` /
`077b23877bc5f60dda864c309765d9980202255574c1162555266527a8354488`.

Before any new measured run, deliberately choose the binary to run on both the
target and serving peers. If returning to the best baseline, rebuild/redeploy
`764ceb050` or the next candidate derived from it, and refresh all serving peers
again before resetting the measurement target.

The isolated worktree `/tmp/zebra-zakura-slottrace` is currently clean at the
rejected commit `53c6c502`. Do not treat that HEAD as a good baseline. The best
baseline is its parent `764ceb050`.

## Where The Investigation Stopped

The next diagnosis was in progress when interrupted:

- `scheduler.next_for_peer()` walks queued ranges in height order and returns
  `NoAssignableRange` before byte-budget checks.
- Trace state for the rejected run showed:
  - large `queue_blocks`
  - huge `needed_count`
  - very low `download_budget_used_pct`
  - `request_slot_available` was 0 during late intervals
  - floor gaps were servable by 8 peers but had 0 available peers
- This suggests the remaining failure is not that peers cannot serve the floor,
  but that all peer request slots can be occupied by later work while the floor
  is queued or outstanding.

Likely next line of attack:

- preserve the timeout route-around behavior from `764ceb050`
- add scheduler/reactor logic that protects the body-download floor from slot
  starvation
- do this without reverting to fanout > 1 and without allowing one slow peer to
  own the floor indefinitely

Concrete things to inspect next:

- `zebra-network/src/zakura/block_sync/reactor.rs`
  - `handle_timeouts`
  - `schedule`
  - `floor_gap_snapshot`
  - trace fields around available slots and outstanding floor peer
- `zebra-network/src/zakura/block_sync/scheduler.rs`
  - `next_for_peer`
  - `can_assign_peer_to_range`
  - `retry_from_peer`
- `zebra-network/src/zakura/block_sync/state.rs`
  - `PeerBlockState::available_slots`
  - `reduce_outbound_window_after_timeout`
  - `increase_outbound_window_after_success`

## Operational Notes

Runbook: `P2P_OPTIMIZATION_RUNBOOK.md`.

Use the analyzer as the oracle:

```bash
python /home/evan/src/valar/zebra.zakura-fixes/analysis/zakura_trace_analysis/analyze_zakura_sync.py \
  --trace-dir /path/to/run/zakura \
  --out /path/to/run/analysis \
  --refresh-cache
```

Collect the whole trace directory, not selected files. The helper used in this
round was `/tmp/zakura_ops.py`.

The helper's `prepare-upload-deploy` deploys only the measurement target. Serving
peers still need a separate state-preserving binary refresh when serving caps or
block-sync behavior changes. The runbook records all eight peer IPs.

## Do Not Use

Do not continue from `53c6c502` as if it were accepted. It is useful only as a
negative result showing that transient retry exclusions are not the budget-fill
fix and can regress HoL.

Do not use the earlier rejected deadline-wakeup experiment `a602bcd0`; it
regressed and is not part of the current best path.
