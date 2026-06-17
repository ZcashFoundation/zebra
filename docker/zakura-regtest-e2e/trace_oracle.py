#!/usr/bin/env python3
"""Zakura regtest trace oracle.

The oracle reads the per-node JSONL trace layout produced by the Docker e2e:

    node*/commit_state.jsonl
    node*/block_sync.jsonl
    node*/header_sync.jsonl

It checks high-signal sync invariants and prints compact diagnostics for the
first offending row in each node. It intentionally depends only on Python's
standard library so it can run from CI failure traps.
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


COMMIT_START = "commit_start"
COMMIT_STALLED = "commit_stalled"
COMMIT_FINISH = "commit_finish"
SYNC_FRONTIER_TRANSITION = "sync_frontier_transition"

BLOCK_GET_BLOCKS_SENT = "block_get_blocks_sent"
BLOCK_BODY_RECEIVED = "block_body_received"
BLOCK_APPLY_FINISHED = "block_apply_finished"
BLOCK_FRONTIERS_CHANGED = "block_frontiers_changed"
BLOCK_CHAIN_TIP_RESET = "block_chain_tip_reset"
BLOCK_SYNC_STATE = "block_sync_state"

HEADER_FRONTIER_ADVANCED = "header_frontier_advanced"
HEADER_FRONTIER_REANCHORED = "header_frontier_reanchored"
HEADER_GET_HEADERS_SENT = "header_get_headers_sent"
HEADER_RANGE_COMMITTED = "header_range_committed"
HEADER_RANGE_REJECTED = "header_range_rejected"
HEADER_PEER_VIOLATION = "header_peer_violation"

QUERY_NEEDED_BLOCKS = "query_needed_blocks"

RESET_CAUSES = {"verified_reset"}
BODY_PROGRESS_EVENTS = {
    BLOCK_GET_BLOCKS_SENT,
    BLOCK_BODY_RECEIVED,
    BLOCK_APPLY_FINISHED,
    BLOCK_FRONTIERS_CHANGED,
    BLOCK_CHAIN_TIP_RESET,
}
RECENT_ACTIVITY_EVENTS = 2_000
RECENT_ACTIVITY_MICROS = 180 * 1_000_000
COMMIT_EVENT_WINDOW = 200_000
COMMIT_MICROS_WINDOW = 30 * 60 * 1_000_000
COMMIT_TREND_MIN_MS = 300_000
COMMIT_TREND_FACTOR = 4.0
APPLY_CLASS_CHECKPOINT = "checkpoint"
APPLY_CLASS_FULL = "full"


@dataclass(frozen=True)
class TraceRow:
    node: str
    table: str
    index: int
    row: dict[str, Any]

    @property
    def event(self) -> str:
        return str(self.row.get("event", ""))

    @property
    def ts(self) -> int | None:
        return int_field(self.row, "ts")


@dataclass
class Failure:
    node: str
    invariant: str
    row: TraceRow | None
    detail: dict[str, Any]


@dataclass(frozen=True)
class OracleOptions:
    commit_event_window: int = COMMIT_EVENT_WINDOW
    commit_elapsed_micros: int = COMMIT_MICROS_WINDOW
    persistent_lag_micros: int = RECENT_ACTIVITY_MICROS
    require_handoff_boundary: bool = False
    handoff_stall_micros: int = RECENT_ACTIVITY_MICROS
    commit_trend_min_ms: int = COMMIT_TREND_MIN_MS
    commit_trend_factor: float = COMMIT_TREND_FACTOR


class NodeTrace:
    def __init__(self, node: str, tables: dict[str, list[TraceRow]]) -> None:
        self.node = node
        self.tables = tables
        self.rows = sorted(
            [row for rows in tables.values() for row in rows],
            key=lambda row: (row.ts if row.ts is not None else -1, row.table, row.index),
        )

    def table(self, name: str) -> list[TraceRow]:
        return self.tables.get(name, [])

    def events(self, table: str, event: str) -> list[TraceRow]:
        return [row for row in self.table(table) if row.event == event]

    def latest(self, table: str, event: str | None = None) -> TraceRow | None:
        rows = self.table(table)
        if event is not None:
            rows = [row for row in rows if row.event == event]
        return rows[-1] if rows else None


def int_field(row: dict[str, Any], field: str) -> int | None:
    value = row.get(field)
    if value is None:
        return None
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float) and value.is_integer():
        return int(value)
    if isinstance(value, str):
        try:
            return int(value)
        except ValueError:
            return None
    return None


def row_key(row: TraceRow) -> tuple[str, Any] | None:
    token = int_field(row.row, "apply_token")
    if token is not None:
        return ("token", token)

    height = int_field(row.row, "height")
    block_hash = row.row.get("hash")
    if height is not None and block_hash is not None:
        return ("height_hash", (height, block_hash))

    action = row.row.get("action")
    range_start = int_field(row.row, "range_start")
    range_count = int_field(row.row, "range_count")
    if action is not None and range_start is not None and range_count is not None:
        return ("action_range", (action, range_start, range_count))

    return None


def compact_row(row: TraceRow | None) -> dict[str, Any] | None:
    if row is None:
        return None

    return {
        "table": row.table,
        "index": row.index,
        "event": row.event,
        "ts": row.ts,
        "row": row.row,
    }


def read_jsonl(path: Path, node: str, table: str) -> list[TraceRow]:
    if not path.exists():
        return []

    rows: list[TraceRow] = []
    with path.open("r", encoding="utf-8") as handle:
        for index, line in enumerate(handle, start=1):
            line = line.strip()
            if not line:
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError as error:
                rows.append(
                    TraceRow(
                        node,
                        table,
                        index,
                        {
                            "event": "json_decode_error",
                            "path": str(path),
                            "line": line[:400],
                            "error": str(error),
                        },
                    )
                )
                continue
            if isinstance(value, dict):
                rows.append(TraceRow(node, table, index, value))
    return rows


def load_traces(root: Path) -> list[NodeTrace]:
    nodes: list[NodeTrace] = []
    node_dirs = sorted(path for path in root.iterdir() if path.is_dir() and path.name.startswith("node"))
    if not node_dirs and any(root.joinpath(name).exists() for name in trace_files()):
        node_dirs = [root]

    for node_dir in node_dirs:
        candidates = [node_dir]
        if not any(node_dir.joinpath(name).exists() for name in trace_files()):
            candidates = [
                child
                for child in sorted(path for path in node_dir.iterdir() if path.is_dir())
                if any(child.joinpath(name).exists() for name in trace_files())
            ]

        for trace_dir in candidates:
            node = node_dir.name if trace_dir == node_dir else f"{node_dir.name}/{trace_dir.name}"
            tables = {
                "commit_state": read_jsonl(trace_dir / "commit_state.jsonl", node, "commit_state"),
                "block_sync": read_jsonl(trace_dir / "block_sync.jsonl", node, "block_sync"),
                "header_sync": read_jsonl(trace_dir / "header_sync.jsonl", node, "header_sync"),
            }
            if any(tables.values()):
                nodes.append(NodeTrace(node, tables))

    return nodes


def trace_files() -> Iterable[str]:
    yield "commit_state.jsonl"
    yield "block_sync.jsonl"
    yield "header_sync.jsonl"


def check_commit_pairs(node: NodeTrace, options: OracleOptions) -> list[Failure]:
    failures: list[Failure] = []
    finishes: dict[tuple[str, Any], list[TraceRow]] = {}
    for finish in node.events("commit_state", COMMIT_FINISH):
        key = row_key(finish)
        if key is not None:
            finishes.setdefault(key, []).append(finish)

    for start in node.events("commit_state", COMMIT_START):
        key = row_key(start)
        if key is None:
            failures.append(
                failure(node, "commit_start_has_match_key", start, {"reason": "missing apply_token and height/hash"})
            )
            continue

        candidates = [
            finish
            for finish in finishes.get(key, [])
            if finish.index > start.index or later_ts(finish, start)
        ]
        if not candidates:
            failures.append(failure(node, "commit_start_has_finish", start, {"key": key}))
            continue

        finish = candidates[0]
        if finish.index - start.index > options.commit_event_window:
            failures.append(
                failure(
                    node,
                    "commit_finish_within_event_window",
                    start,
                    {"key": key, "finish": compact_row(finish), "event_delta": finish.index - start.index},
                )
            )
        elif start.ts is not None and finish.ts is not None and finish.ts - start.ts > options.commit_elapsed_micros:
            failures.append(
                failure(
                    node,
                    "commit_finish_within_elapsed_window",
                    start,
                    {"key": key, "finish": compact_row(finish), "elapsed_us": finish.ts - start.ts},
                )
            )
        else:
            elapsed_ms = int_field(finish.row, "elapsed_ms")
            if elapsed_ms is not None and elapsed_ms * 1_000 > options.commit_elapsed_micros:
                failures.append(
                    failure(
                        node,
                        "commit_finish_within_elapsed_window",
                        start,
                        {"key": key, "finish": compact_row(finish), "elapsed_ms": elapsed_ms},
                    )
                )

    for stalled in node.events("commit_state", COMMIT_STALLED):
        key = row_key(stalled)
        later_finish = [
            finish
            for finish in finishes.get(key, [])
            if key is not None and (finish.index > stalled.index or later_ts(finish, stalled))
        ]
        if not later_finish:
            failures.append(failure(node, "commit_stalled_not_terminal", stalled, {"key": key}))

    failures.extend(check_commit_latency_trend(node, options))
    return failures


def check_commit_latency_trend(node: NodeTrace, options: OracleOptions) -> list[Failure]:
    failures: list[Failure] = []
    elapsed_by_class: dict[str, list[tuple[TraceRow, int]]] = {}

    for row in node.events("commit_state", COMMIT_FINISH):
        elapsed_ms = int_field(row.row, "elapsed_ms")
        if elapsed_ms is None:
            continue

        commit_class = str(row.row.get("apply_class") or row.row.get("action") or "unknown")
        elapsed_by_class.setdefault(commit_class, []).append((row, elapsed_ms))

    for commit_class, samples in elapsed_by_class.items():
        if len(samples) < 3:
            continue

        for previous, current in zip(samples, samples[1:]):
            previous_row, previous_ms = previous
            current_row, current_ms = current
            if previous_ms <= 0:
                continue
            if current_ms < options.commit_trend_min_ms:
                continue
            if current_ms >= int(previous_ms * options.commit_trend_factor):
                failures.append(
                    failure(
                        node,
                        "commit_latency_trend_bounded",
                        current_row,
                        {
                            "commit_class": commit_class,
                            "previous": compact_row(previous_row),
                            "previous_elapsed_ms": previous_ms,
                            "current_elapsed_ms": current_ms,
                            "trend_factor": options.commit_trend_factor,
                        },
                    )
                )
                break

        first_row, first_ms = samples[0]
        last_row, last_ms = samples[-1]
        if (
            first_ms > 0
            and last_ms >= options.commit_trend_min_ms
            and last_ms >= int(first_ms * options.commit_trend_factor)
            and nondecreasing([elapsed_ms for _, elapsed_ms in samples])
            and not any(
                f.invariant == "commit_latency_trend_bounded"
                and f.detail.get("commit_class") == commit_class
                for f in failures
            )
        ):
            failures.append(
                failure(
                    node,
                    "commit_latency_trend_bounded",
                    last_row,
                    {
                        "commit_class": commit_class,
                        "first": compact_row(first_row),
                        "first_elapsed_ms": first_ms,
                        "last_elapsed_ms": last_ms,
                        "trend_factor": options.commit_trend_factor,
                    },
                )
            )

    return failures


def nondecreasing(values: list[int]) -> bool:
    return all(right >= left for left, right in zip(values, values[1:]))


def later_ts(candidate: TraceRow, previous: TraceRow) -> bool:
    return (
        candidate.ts is not None
        and previous.ts is not None
        and candidate.ts >= previous.ts
        and candidate is not previous
    )


def check_frontiers(node: NodeTrace) -> list[Failure]:
    failures: list[Failure] = []
    for row in node.events("commit_state", SYNC_FRONTIER_TRANSITION):
        old_finalized = int_field(row.row, "old_finalized_height")
        new_finalized = int_field(row.row, "new_finalized_height")
        if old_finalized is not None and new_finalized is not None and new_finalized < old_finalized:
            failures.append(
                failure(
                    node,
                    "finalized_height_monotonic",
                    row,
                    {"old_finalized_height": old_finalized, "new_finalized_height": new_finalized},
                )
            )

        old_verified = int_field(row.row, "old_verified_body_height")
        new_verified = int_field(row.row, "new_verified_body_height")
        cause = str(row.row.get("cause", ""))
        if (
            old_verified is not None
            and new_verified is not None
            and new_verified < old_verified
            and cause not in RESET_CAUSES
        ):
            failures.append(
                failure(
                    node,
                    "verified_body_height_monotonic_except_reset",
                    row,
                    {
                        "cause": cause,
                        "old_verified_body_height": old_verified,
                        "new_verified_body_height": new_verified,
                    },
                )
            )
        if cause == "header_reanchored":
            old_header = int_field(row.row, "old_best_header_height")
            new_header = int_field(row.row, "new_best_header_height")
            if old_verified is not None and new_verified is not None and new_verified < old_verified:
                failures.append(failure(node, "header_reanchor_did_not_lower_verified_body", row, {}))
            if old_finalized is not None and new_finalized is not None and new_finalized < old_finalized:
                failures.append(failure(node, "header_reanchor_did_not_lower_finalized", row, {}))
            if old_header is not None and new_header is not None and new_header > old_header:
                failures.append(
                    failure(
                        node,
                        "header_reanchor_only_lowers_best_header",
                        row,
                        {"old_best_header_height": old_header, "new_best_header_height": new_header},
                    )
                )

    return failures


def check_block_sync_activity(node: NodeTrace, options: OracleOptions) -> list[Failure]:
    failures: list[Failure] = []
    states = node.events("block_sync", BLOCK_SYNC_STATE)
    real_activity = sorted(
        [row for row in node.table("block_sync") if row.event in BODY_PROGRESS_EVENTS]
        + [row for row in node.table("commit_state") if commit_state_row_is_body_progress(row)],
        key=sort_key,
    )
    query_rows = [
        row
        for row in node.table("commit_state")
        if row.row.get("action") == QUERY_NEEDED_BLOCKS
        and row.event in {"state_read_start", "state_read_success", "state_read_error", "state_read_timeout"}
    ]

    for state in states:
        best = int_field(state.row, "best_header_tip")
        verified = int_field(state.row, "verified_block_tip")
        if best is None or verified is None or best <= verified:
            continue

        has_real_activity = has_near_activity(state, real_activity, options)
        if not has_real_activity:
            recent_query_count = len(rows_near(state, query_rows, options))
            invariant = (
                "lagging_body_sync_not_query_spin"
                if recent_query_count > 0
                else "lagging_body_sync_has_real_activity"
            )
            failures.append(
                failure(
                    node,
                    invariant,
                    state,
                    {
                        "best_header_tip": best,
                        "verified_block_tip": verified,
                        "recent_query_needed_blocks": recent_query_count,
                        "nearby_real_activity": [compact_row(row) for row in rows_near(state, real_activity, options)[-5:]],
                    },
                )
            )
            break

        if body_sync_has_pinned_queue(state) and not has_forward_activity(state, real_activity, options):
            failures.append(
                failure(
                    node,
                    "block_sync_queue_lag_makes_progress",
                    state,
                    {
                        "best_header_tip": best,
                        "verified_block_tip": verified,
                        "needed_count": int_field(state.row, "needed_count"),
                        "queue_len": int_field(state.row, "queue_len"),
                        "assigned_len": int_field(state.row, "assigned_len"),
                        "outstanding": int_field(state.row, "outstanding"),
                    },
                )
            )
            break

    final_state = states[-1] if states else None
    if final_state is not None:
        leaked = {
            name: int_field(final_state.row, name)
            for name in ("budget_reserved", "reorder", "applying", "outstanding")
        }
        bad = {name: value for name, value in leaked.items() if value is not None and value != 0}
        if bad:
            failures.append(failure(node, "final_block_sync_state_has_no_leaks", final_state, {"leaked": bad}))

    return failures


def sort_key(row: TraceRow) -> tuple[int, str, int]:
    return (row.ts if row.ts is not None else -1, row.table, row.index)


def commit_state_row_is_body_progress(row: TraceRow) -> bool:
    if row.event == SYNC_FRONTIER_TRANSITION:
        old_verified = int_field(row.row, "old_verified_body_height")
        new_verified = int_field(row.row, "new_verified_body_height")
        cause = str(row.row.get("cause", ""))
        return (
            old_verified is not None
            and new_verified is not None
            and (new_verified > old_verified or cause in RESET_CAUSES)
        )
    if row.event == COMMIT_FINISH and row.row.get("source") == "block_sync_driver":
        return True
    return False


def rows_near(state: TraceRow, activity: list[TraceRow], options: OracleOptions) -> list[TraceRow]:
    state_ts = state.ts
    if state_ts is None:
        lower = max(0, state.index - RECENT_ACTIVITY_EVENTS)
        upper = state.index + RECENT_ACTIVITY_EVENTS
        return [row for row in activity if row.table != state.table or lower <= row.index <= upper]

    return [
        row
        for row in activity
        if row.ts is not None and abs(row.ts - state_ts) <= options.persistent_lag_micros
    ]


def has_near_activity(state: TraceRow, activity: list[TraceRow], options: OracleOptions) -> bool:
    return bool(rows_near(state, activity, options))


def has_forward_activity(state: TraceRow, activity: list[TraceRow], options: OracleOptions) -> bool:
    state_ts = state.ts
    if state_ts is None:
        return any(
            row.table != state.table or state.index <= row.index <= state.index + RECENT_ACTIVITY_EVENTS
            for row in activity
        )
    return any(
        row.ts is not None
        and state_ts <= row.ts <= state_ts + options.persistent_lag_micros
        for row in activity
    )


def body_sync_has_pinned_queue(state: TraceRow) -> bool:
    return any(
        (int_field(state.row, field) or 0) > 0
        for field in ("needed_count", "queue_len", "assigned_len", "outstanding")
    )


def check_header_recovery(node: NodeTrace, options: OracleOptions) -> list[Failure]:
    failures: list[Failure] = []
    recoveries = [
        row
        for row in node.table("header_sync")
        if row.event in {HEADER_FRONTIER_REANCHORED, HEADER_FRONTIER_ADVANCED, HEADER_RANGE_COMMITTED, HEADER_GET_HEADERS_SENT}
    ]

    for rejected in node.events("header_sync", HEADER_RANGE_REJECTED):
        stage = str(rejected.row.get("validation_stage", ""))
        error = str(rejected.row.get("error_kind", ""))
        if stage != "link" and error != "first_header_does_not_link":
            continue

        if header_reject_has_recovery(rejected, recoveries, options):
            continue
        if not trace_extends_past(node, rejected, options.persistent_lag_micros):
            continue

        failures.append(
            failure(
                node,
                "header_link_reject_recovers",
                rejected,
                {
                    "validation_stage": stage,
                    "error_kind": error,
                    "range_start": int_field(rejected.row, "range_start"),
                    "anchor_hash": rejected.row.get("anchor_hash"),
                },
            )
        )
        break

    return failures


def header_reject_has_recovery(rejected: TraceRow, recoveries: list[TraceRow], options: OracleOptions) -> bool:
    start = int_field(rejected.row, "range_start")
    rejected_ts = rejected.ts

    for row in recoveries:
        if rejected_ts is not None and row.ts is not None:
            if not (rejected_ts <= row.ts <= rejected_ts + options.persistent_lag_micros):
                continue
        elif row.index <= rejected.index or row.index > rejected.index + RECENT_ACTIVITY_EVENTS:
            continue

        if row.event == HEADER_FRONTIER_REANCHORED:
            return True
        if row.event == HEADER_RANGE_COMMITTED:
            return True
        if row.event == HEADER_FRONTIER_ADVANCED:
            height = int_field(row.row, "height")
            if start is None or height is None or height >= start:
                return True
        if row.event == HEADER_GET_HEADERS_SENT:
            next_start = int_field(row.row, "range_start")
            if start is None or next_start is None or next_start >= start:
                return True

    return False


def trace_extends_past(node: NodeTrace, row: TraceRow, micros: int) -> bool:
    if row.ts is None:
        return True
    latest_ts = max((candidate.ts for candidate in node.rows if candidate.ts is not None), default=row.ts)
    return latest_ts - row.ts >= micros


def check_checkpoint_to_full_handoff(nodes: list[NodeTrace], options: OracleOptions) -> list[Failure]:
    observations = [checkpoint_to_full_handoff(node, options) for node in nodes]
    if any(observation[0] is None for observation in observations):
        return []

    concrete_failures = [
        failure
        for failure, _ in observations
        if failure is not None and failure.invariant != "checkpoint_to_full_handoff_observed"
    ]
    if concrete_failures:
        return concrete_failures[:1]

    diagnostics_by_node = {node.node: diagnostics(node) for node in nodes}
    best_detail = next((detail for _, detail in observations if detail), {})
    return [
        Failure(
            "<all>",
            "checkpoint_to_full_handoff_observed",
            None,
            {
                **best_detail,
                "diagnostics": diagnostics_by_node,
            },
        )
    ]


def checkpoint_to_full_handoff(
    node: NodeTrace, options: OracleOptions
) -> tuple[Failure | None, dict[str, Any]]:
    checkpoint_finish: TraceRow | None = None
    checkpoint_height: int | None = None

    for row in node.events("commit_state", COMMIT_FINISH):
        apply_class = row.row.get("apply_class")
        height = int_field(row.row, "height")
        if apply_class == APPLY_CLASS_CHECKPOINT and height is not None:
            checkpoint_finish = row
            checkpoint_height = height
        elif (
            apply_class == APPLY_CLASS_FULL
            and checkpoint_finish is not None
            and height is not None
            and checkpoint_height is not None
            and height > checkpoint_height
            and (row.index > checkpoint_finish.index or later_ts(row, checkpoint_finish))
        ):
            if checkpoint_finish.ts is not None and row.ts is not None:
                elapsed = row.ts - checkpoint_finish.ts
                if elapsed > options.handoff_stall_micros:
                    return (
                        failure(
                            node,
                            "checkpoint_to_full_handoff_within_window",
                            row,
                            {
                                "checkpoint": compact_row(checkpoint_finish),
                                "full": compact_row(row),
                                "elapsed_us": elapsed,
                            },
                        ),
                        {},
                    )
            return (None, {})

    return (
        failure(
            node,
            "checkpoint_to_full_handoff_observed",
            checkpoint_finish,
            {"latest_checkpoint_height": checkpoint_height},
        ),
        {"latest_checkpoint_height": checkpoint_height},
    )


def failure(node: NodeTrace, invariant: str, row: TraceRow | None, detail: dict[str, Any]) -> Failure:
    return Failure(
        node=node.node,
        invariant=invariant,
        row=row,
        detail={
            **detail,
            "diagnostics": diagnostics(node),
        },
    )


def diagnostics(node: NodeTrace) -> dict[str, Any]:
    starts = node.events("commit_state", COMMIT_START)
    finishes_by_key = {row_key(row) for row in node.events("commit_state", COMMIT_FINISH)}
    unmatched = []
    for start in starts:
        key = row_key(start)
        if key is not None and key not in finishes_by_key:
            unmatched.append({"key": key, "row": compact_row(start)})
        if len(unmatched) >= 8:
            break

    needed_rows = [
        row
        for row in node.table("commit_state")
        if row.row.get("action") in {QUERY_NEEDED_BLOCKS, "needed_blocks"}
    ]

    return {
        "last_frontier_transition": compact_row(node.latest("commit_state", SYNC_FRONTIER_TRANSITION)),
        "unmatched_commit_starts": unmatched,
        "latest_needed_blocks": compact_row(needed_rows[-1] if needed_rows else None),
        "latest_block_sync_state": compact_row(node.latest("block_sync", BLOCK_SYNC_STATE)),
        "recent_peer_violations": [
            compact_row(row) for row in node.events("header_sync", HEADER_PEER_VIOLATION)[-5:]
        ],
        "latest_header_reanchor": compact_row(node.latest("header_sync", HEADER_FRONTIER_REANCHORED)),
    }


def run_oracle(root: Path, options: OracleOptions = OracleOptions()) -> list[Failure]:
    nodes = load_traces(root)
    if not nodes:
        return [Failure("<none>", "trace_files_exist", None, {"root": str(root)})]

    failures: list[Failure] = []
    for node in nodes:
        for row in node.rows:
            if row.event == "json_decode_error":
                failures.append(failure(node, "trace_jsonl_is_valid", row, {}))
        failures.extend(check_commit_pairs(node, options))
        failures.extend(check_frontiers(node))
        failures.extend(check_block_sync_activity(node, options))
        failures.extend(check_header_recovery(node, options))

    if options.require_handoff_boundary:
        failures.extend(check_checkpoint_to_full_handoff(nodes, options))

    return failures


def print_failures(failures: list[Failure]) -> None:
    for item in failures:
        payload = {
            "node": item.node,
            "invariant": item.invariant,
            "first_offending_row": compact_row(item.row),
            **item.detail,
        }
        print("TRACE ORACLE FAILURE", file=sys.stderr)
        print(json.dumps(payload, indent=2, sort_keys=True), file=sys.stderr)


def write_jsonl(path: Path, rows: Iterable[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row, sort_keys=True))
            handle.write("\n")


def run_self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="zakura-trace-oracle.") as tmp:
        root = Path(tmp)

        good = root / "good" / "node1"
        write_jsonl(
            good / "commit_state.jsonl",
            [
                {"ts": 1, "event": COMMIT_START, "apply_token": 1, "height": 1, "hash": "aa"},
                {"ts": 2, "event": COMMIT_FINISH, "apply_token": 1, "height": 1, "hash": "aa"},
                {
                    "ts": 3,
                    "event": SYNC_FRONTIER_TRANSITION,
                    "cause": "verified_grow",
                    "old_finalized_height": 0,
                    "new_finalized_height": 0,
                    "old_verified_body_height": 0,
                    "new_verified_body_height": 1,
                    "old_best_header_height": 1,
                    "new_best_header_height": 1,
                },
            ],
        )
        write_jsonl(
            good / "block_sync.jsonl",
            [
                {
                    "ts": 4,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 1,
                    "best_header_tip": 1,
                    "budget_reserved": 0,
                    "reorder": 0,
                    "applying": 0,
                    "outstanding": 0,
                }
            ],
        )
        assert not run_oracle(root / "good"), "good trace should pass"

        handoff = root / "handoff" / "node2"
        write_jsonl(
            handoff / "commit_state.jsonl",
            [
                {
                    "ts": 1,
                    "event": COMMIT_START,
                    "apply_token": 10,
                    "apply_class": APPLY_CLASS_CHECKPOINT,
                    "height": 80,
                    "hash": "cc",
                },
                {
                    "ts": 2,
                    "event": COMMIT_FINISH,
                    "apply_token": 10,
                    "apply_class": APPLY_CLASS_CHECKPOINT,
                    "height": 80,
                    "hash": "cc",
                    "elapsed_ms": 1,
                },
                {
                    "ts": 3,
                    "event": COMMIT_START,
                    "apply_token": 11,
                    "apply_class": APPLY_CLASS_FULL,
                    "height": 81,
                    "hash": "dd",
                },
                {
                    "ts": 4,
                    "event": COMMIT_FINISH,
                    "apply_token": 11,
                    "apply_class": APPLY_CLASS_FULL,
                    "height": 81,
                    "hash": "dd",
                    "elapsed_ms": 1,
                },
            ],
        )
        assert not run_oracle(
            root / "handoff",
            OracleOptions(require_handoff_boundary=True),
        ), "checkpoint-to-full handoff trace should pass"

        no_handoff = root / "no_handoff" / "node2"
        write_jsonl(
            no_handoff / "commit_state.jsonl",
            [
                {
                    "ts": 1,
                    "event": COMMIT_START,
                    "apply_token": 12,
                    "apply_class": APPLY_CLASS_CHECKPOINT,
                    "height": 80,
                    "hash": "ee",
                },
                {
                    "ts": 2,
                    "event": COMMIT_FINISH,
                    "apply_token": 12,
                    "apply_class": APPLY_CLASS_CHECKPOINT,
                    "height": 80,
                    "hash": "ee",
                },
            ],
        )
        assert any(
            f.invariant == "checkpoint_to_full_handoff_observed"
            for f in run_oracle(root / "no_handoff", OracleOptions(require_handoff_boundary=True))
        )

        slow_handoff = root / "slow_handoff" / "node2"
        write_jsonl(
            slow_handoff / "commit_state.jsonl",
            [
                {
                    "ts": 1,
                    "event": COMMIT_FINISH,
                    "apply_token": 20,
                    "apply_class": APPLY_CLASS_CHECKPOINT,
                    "height": 80,
                    "hash": "aa",
                },
                {
                    "ts": 10_000_000,
                    "event": COMMIT_FINISH,
                    "apply_token": 21,
                    "apply_class": APPLY_CLASS_FULL,
                    "height": 81,
                    "hash": "bb",
                },
            ],
        )
        assert any(
            f.invariant == "checkpoint_to_full_handoff_within_window"
            for f in run_oracle(
                root / "slow_handoff",
                OracleOptions(require_handoff_boundary=True, handoff_stall_micros=1_000_000),
            )
        )

        bad_commit = root / "bad_commit" / "node1"
        write_jsonl(
            bad_commit / "commit_state.jsonl",
            [{"ts": 1, "event": COMMIT_START, "apply_token": 9, "height": 9, "hash": "bb"}],
        )
        assert any(f.invariant == "commit_start_has_finish" for f in run_oracle(root / "bad_commit"))

        terminal_stalled = root / "terminal_stalled" / "node1"
        write_jsonl(
            terminal_stalled / "commit_state.jsonl",
            [
                {
                    "ts": 1,
                    "event": COMMIT_STALLED,
                    "apply_token": 15,
                    "height": 15,
                    "hash": "ff",
                }
            ],
        )
        assert any(f.invariant == "commit_stalled_not_terminal" for f in run_oracle(root / "terminal_stalled"))

        bad_frontier = root / "bad_frontier" / "node1"
        write_jsonl(
            bad_frontier / "commit_state.jsonl",
            [
                {
                    "ts": 1,
                    "event": SYNC_FRONTIER_TRANSITION,
                    "cause": "verified_grow",
                    "old_finalized_height": 10,
                    "new_finalized_height": 9,
                    "old_verified_body_height": 10,
                    "new_verified_body_height": 9,
                }
            ],
        )
        failures = run_oracle(root / "bad_frontier")
        assert any(f.invariant == "finalized_height_monotonic" for f in failures)
        assert any(f.invariant == "verified_body_height_monotonic_except_reset" for f in failures)

        valid_reset = root / "valid_reset" / "node1"
        write_jsonl(
            valid_reset / "commit_state.jsonl",
            [
                {
                    "ts": 1,
                    "event": SYNC_FRONTIER_TRANSITION,
                    "cause": "verified_reset",
                    "old_finalized_height": 10,
                    "new_finalized_height": 10,
                    "old_verified_body_height": 10,
                    "new_verified_body_height": 9,
                }
            ],
        )
        assert not run_oracle(root / "valid_reset"), "valid reorg/reset should pass"

        persistent_lag = root / "persistent_lag" / "node1"
        write_jsonl(
            persistent_lag / "block_sync.jsonl",
            [
                {
                    "ts": 1,
                    "event": BLOCK_GET_BLOCKS_SENT,
                    "range_start": 1,
                    "range_count": 1,
                },
                {
                    "ts": 500_000_000,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 1,
                    "best_header_tip": 10,
                    "budget_reserved": 0,
                    "reorder": 0,
                    "applying": 0,
                    "outstanding": 0,
                },
            ],
        )
        assert any(
            f.invariant == "lagging_body_sync_has_real_activity"
            for f in run_oracle(root / "persistent_lag", OracleOptions(persistent_lag_micros=1_000_000))
        )

        query_spin = root / "query_spin" / "node1"
        write_jsonl(
            query_spin / "commit_state.jsonl",
            [
                {"ts": 1_000_000, "event": "state_read_start", "action": QUERY_NEEDED_BLOCKS},
                {
                    "ts": 2_000_000,
                    "event": "state_read_success",
                    "action": QUERY_NEEDED_BLOCKS,
                    "range_count": 0,
                },
                {"ts": 3_000_000, "event": "state_read_start", "action": QUERY_NEEDED_BLOCKS},
                {
                    "ts": 4_000_000,
                    "event": "state_read_success",
                    "action": QUERY_NEEDED_BLOCKS,
                    "range_count": 0,
                },
            ],
        )
        write_jsonl(
            query_spin / "block_sync.jsonl",
            [
                {
                    "ts": 5_000_000,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 10,
                    "best_header_tip": 20,
                    "budget_reserved": 0,
                    "reorder": 0,
                    "applying": 0,
                    "outstanding": 0,
                    "needed_count": 0,
                    "queue_len": 0,
                    "assigned_len": 0,
                }
            ],
        )
        assert any(
            f.invariant == "lagging_body_sync_not_query_spin"
            for f in run_oracle(root / "query_spin", OracleOptions(persistent_lag_micros=10_000_000))
        )

        self_healed_wedge = root / "self_healed_wedge" / "node1"
        write_jsonl(
            self_healed_wedge / "block_sync.jsonl",
            [
                {
                    "ts": 1,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 10,
                    "best_header_tip": 30,
                    "budget_reserved": 0,
                    "reorder": 0,
                    "applying": 0,
                    "outstanding": 0,
                    "needed_count": 0,
                    "queue_len": 0,
                    "assigned_len": 0,
                },
                {
                    "ts": 200_000_000,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 10,
                    "best_header_tip": 30,
                    "budget_reserved": 0,
                    "reorder": 0,
                    "applying": 0,
                    "outstanding": 0,
                    "needed_count": 0,
                    "queue_len": 0,
                    "assigned_len": 0,
                },
                {"ts": 400_000_000, "event": BLOCK_GET_BLOCKS_SENT, "range_start": 11, "range_count": 20},
                {
                    "ts": 401_000_000,
                    "event": BLOCK_FRONTIERS_CHANGED,
                    "verified_block_tip": 30,
                    "best_header_tip": 30,
                },
                {
                    "ts": 402_000_000,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 30,
                    "best_header_tip": 30,
                    "budget_reserved": 0,
                    "reorder": 0,
                    "applying": 0,
                    "outstanding": 0,
                },
            ],
        )
        assert any(
            f.invariant == "lagging_body_sync_has_real_activity"
            for f in run_oracle(root / "self_healed_wedge", OracleOptions(persistent_lag_micros=30_000_000))
        )

        lag_with_progress = root / "lag_with_progress" / "node1"
        write_jsonl(
            lag_with_progress / "block_sync.jsonl",
            [
                {"ts": 10, "event": BLOCK_GET_BLOCKS_SENT, "range_start": 2, "range_count": 4},
                {
                    "ts": 20,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 1,
                    "best_header_tip": 5,
                    "budget_reserved": 0,
                    "reorder": 0,
                    "applying": 0,
                    "outstanding": 0,
                    "needed_count": 4,
                    "queue_len": 0,
                    "assigned_len": 0,
                },
                {"ts": 30, "event": BLOCK_BODY_RECEIVED, "height": 2},
                {
                    "ts": 40,
                    "event": BLOCK_FRONTIERS_CHANGED,
                    "verified_block_tip": 2,
                    "best_header_tip": 5,
                },
                {
                    "ts": 50,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 5,
                    "best_header_tip": 5,
                    "budget_reserved": 0,
                    "reorder": 0,
                    "applying": 0,
                    "outstanding": 0,
                },
            ],
        )
        assert not run_oracle(root / "lag_with_progress", OracleOptions(persistent_lag_micros=1_000_000))

        queued_lag = root / "queued_lag" / "node1"
        write_jsonl(
            queued_lag / "block_sync.jsonl",
            [
                {"ts": 9_500_000, "event": BLOCK_GET_BLOCKS_SENT, "range_start": 11, "range_count": 1},
                {
                    "ts": 10_000_000,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 10,
                    "best_header_tip": 20,
                    "budget_reserved": 0,
                    "reorder": 0,
                    "applying": 0,
                    "outstanding": 1,
                    "needed_count": 10,
                    "queue_len": 1,
                    "assigned_len": 1,
                },
            ],
        )
        assert any(
            f.invariant == "block_sync_queue_lag_makes_progress"
            for f in run_oracle(root / "queued_lag", OracleOptions(persistent_lag_micros=1_000_000))
        )

        latency_trend = root / "latency_trend" / "node1"
        write_jsonl(
            latency_trend / "commit_state.jsonl",
            [
                {
                    "ts": 1,
                    "event": COMMIT_FINISH,
                    "apply_token": 1,
                    "apply_class": APPLY_CLASS_FULL,
                    "height": 1,
                    "hash": "aa",
                    "elapsed_ms": 440_000,
                },
                {
                    "ts": 2,
                    "event": COMMIT_FINISH,
                    "apply_token": 2,
                    "apply_class": APPLY_CLASS_FULL,
                    "height": 2,
                    "hash": "bb",
                    "elapsed_ms": 2_020_000,
                },
                {
                    "ts": 3,
                    "event": COMMIT_FINISH,
                    "apply_token": 3,
                    "apply_class": APPLY_CLASS_FULL,
                    "height": 3,
                    "hash": "cc",
                    "elapsed_ms": 2_100_000,
                },
            ],
        )
        assert any(
            f.invariant == "commit_latency_trend_bounded"
            for f in run_oracle(
                root / "latency_trend",
                OracleOptions(commit_elapsed_micros=3_000_000_000, commit_trend_min_ms=300_000),
            )
        )

        stranded_anchor = root / "stranded_anchor" / "node1"
        write_jsonl(
            stranded_anchor / "header_sync.jsonl",
            [
                {
                    "ts": 1,
                    "event": HEADER_RANGE_REJECTED,
                    "range_start": 100,
                    "range_count": 1,
                    "anchor_hash": "aa",
                    "validation_stage": "link",
                    "error_kind": "first_header_does_not_link",
                },
                {"ts": 2_000_000, "event": HEADER_PEER_VIOLATION, "reason": "invalid_range"},
            ],
        )
        assert any(
            f.invariant == "header_link_reject_recovers"
            for f in run_oracle(root / "stranded_anchor", OracleOptions(persistent_lag_micros=1_000_000))
        )

        recovered_anchor = root / "recovered_anchor" / "node1"
        write_jsonl(
            recovered_anchor / "header_sync.jsonl",
            [
                {
                    "ts": 1,
                    "event": HEADER_RANGE_REJECTED,
                    "range_start": 100,
                    "range_count": 1,
                    "anchor_hash": "aa",
                    "validation_stage": "link",
                    "error_kind": "first_header_does_not_link",
                },
                {"ts": 2, "event": HEADER_FRONTIER_REANCHORED, "height": 99},
            ],
        )
        assert not run_oracle(root / "recovered_anchor", OracleOptions(persistent_lag_micros=1_000_000))

        bad_leak = root / "bad_leak" / "node1"
        write_jsonl(
            bad_leak / "block_sync.jsonl",
            [
                {
                    "ts": 1,
                    "event": BLOCK_SYNC_STATE,
                    "verified_block_tip": 5,
                    "best_header_tip": 5,
                    "budget_reserved": 42,
                    "reorder": 0,
                    "applying": 1,
                    "outstanding": 0,
                }
            ],
        )
        assert any(f.invariant == "final_block_sync_state_has_no_leaks" for f in run_oracle(root / "bad_leak"))

    print("trace_oracle self-test: PASS")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace_dir", nargs="?", type=Path, help="root trace directory containing node* subdirectories")
    parser.add_argument("--self-test", action="store_true", help="run fixture-based oracle tests")
    parser.add_argument(
        "--commit-elapsed-ms",
        type=int,
        default=COMMIT_MICROS_WINDOW // 1_000,
        help="maximum allowed elapsed time between commit_start and commit_finish",
    )
    parser.add_argument(
        "--persistent-lag-seconds",
        type=int,
        default=RECENT_ACTIVITY_MICROS // 1_000_000,
        help="body/header lag must have real get-blocks or body progress within this window",
    )
    parser.add_argument(
        "--handoff-stall-seconds",
        type=int,
        default=RECENT_ACTIVITY_MICROS // 1_000_000,
        help="maximum allowed gap between a checkpoint commit and the first higher full commit",
    )
    parser.add_argument(
        "--commit-trend-min-ms",
        type=int,
        default=COMMIT_TREND_MIN_MS,
        help="minimum commit latency before trend degradation is actionable",
    )
    parser.add_argument(
        "--commit-trend-factor",
        type=float,
        default=COMMIT_TREND_FACTOR,
        help="maximum allowed step-up or monotonic first-to-last commit latency factor",
    )
    parser.add_argument(
        "--require-handoff-boundary",
        action="store_true",
        help="require at least one checkpoint commit followed by a higher full-verifier commit",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.self_test:
        run_self_test()
        return 0

    if args.trace_dir is None:
        print("trace_oracle.py: trace_dir is required unless --self-test is used", file=sys.stderr)
        return 2

    failures = run_oracle(
        args.trace_dir,
        OracleOptions(
            commit_elapsed_micros=args.commit_elapsed_ms * 1_000,
            persistent_lag_micros=args.persistent_lag_seconds * 1_000_000,
            require_handoff_boundary=args.require_handoff_boundary,
            handoff_stall_micros=args.handoff_stall_seconds * 1_000_000,
            commit_trend_min_ms=args.commit_trend_min_ms,
            commit_trend_factor=args.commit_trend_factor,
        ),
    )
    if failures:
        print_failures(failures)
        return 1

    print(f"trace_oracle: PASS ({args.trace_dir})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
