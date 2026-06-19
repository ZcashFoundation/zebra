#!/usr/bin/env python3
"""Analyze Zakura block-sync JSONL traces."""

from __future__ import annotations

import argparse
import json
import math
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import duckdb
import matplotlib

matplotlib.use("Agg")

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd


DEFAULT_TRACE_DIR = Path(
    "/home/evan/src/valar/art/inbox/zakura-blocksync-traces-latest/asia-pacific-0/zakura"
)
DEFAULT_OUT_DIR = Path("analysis/zakura_trace_analysis/out")
SCHEMA_VERSION = 3


@dataclass(frozen=True)
class TracePaths:
    block_sync: Path
    commit_state: Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Analyze Zakura block sync throughput from JSONL traces."
    )
    parser.add_argument("--trace-dir", type=Path, default=DEFAULT_TRACE_DIR)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT_DIR)
    parser.add_argument("--bucket-seconds", type=int, default=60)
    parser.add_argument(
        "--refresh-cache",
        action="store_true",
        help="Rebuild the cached DuckDB tables even when trace metadata matches.",
    )
    return parser.parse_args()


def trace_paths(trace_dir: Path) -> TracePaths:
    paths = TracePaths(
        block_sync=trace_dir / "block_sync.jsonl",
        commit_state=trace_dir / "commit_state.jsonl",
    )
    missing = [str(path) for path in (paths.block_sync, paths.commit_state) if not path.is_file()]
    if missing:
        raise FileNotFoundError("missing required trace files: " + ", ".join(missing))
    return paths


def file_metadata(paths: TracePaths) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "block_sync": _path_metadata(paths.block_sync),
        "commit_state": _path_metadata(paths.commit_state),
    }


def _path_metadata(path: Path) -> dict[str, Any]:
    stat = path.stat()
    return {
        "path": str(path.resolve()),
        "size": stat.st_size,
        "mtime_ns": stat.st_mtime_ns,
    }


def cache_is_valid(con: duckdb.DuckDBPyConnection, expected_metadata: dict[str, Any]) -> bool:
    try:
        row = con.execute(
            "SELECT metadata_json FROM cache_metadata WHERE key = 'trace_metadata'"
        ).fetchone()
    except duckdb.CatalogException:
        return False
    if row is None:
        return False
    return json.loads(row[0]) == expected_metadata


def build_cache(
    trace_dir: Path,
    out_dir: Path,
    *,
    refresh_cache: bool = False,
) -> Path:
    paths = trace_paths(trace_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    db_path = out_dir / "zakura_trace_cache.duckdb"
    metadata = file_metadata(paths)

    con = duckdb.connect(str(db_path))
    try:
        if not refresh_cache and cache_is_valid(con, metadata):
            return db_path

        con.execute("DROP TABLE IF EXISTS block_bodies")
        con.execute("DROP TABLE IF EXISTS block_body_first_seen")
        con.execute("DROP TABLE IF EXISTS block_sync_states")
        con.execute("DROP TABLE IF EXISTS frontier_transitions")
        con.execute("DROP TABLE IF EXISTS block_get_blocks_sent")
        con.execute("DROP TABLE IF EXISTS block_message_sent")
        con.execute("DROP TABLE IF EXISTS block_message_received")
        con.execute("DROP TABLE IF EXISTS block_status_received")
        con.execute("DROP TABLE IF EXISTS block_range_response_sent")
        con.execute("DROP TABLE IF EXISTS cache_metadata")

        con.execute(
            """
            CREATE TABLE block_bodies AS
            SELECT
                TRY_CAST(json_extract_string(json, '$.ts') AS BIGINT) AS ts,
                TRY_CAST(json_extract_string(json, '$.height') AS BIGINT) AS height,
                TRY_CAST(json_extract_string(json, '$.serialized_bytes') AS BIGINT)
                    AS serialized_bytes,
                json_extract_string(json, '$.peer') AS peer
            FROM read_json_objects(?)
            WHERE json_extract_string(json, '$.event') = 'block_body_received'
            """,
            [str(paths.block_sync)],
        )
        con.execute(
            """
            CREATE TABLE block_body_first_seen AS
            SELECT height, ts, serialized_bytes
            FROM (
                SELECT
                    height,
                    ts,
                    serialized_bytes,
                    row_number() OVER (PARTITION BY height ORDER BY ts ASC) AS rn
                FROM block_bodies
                WHERE height IS NOT NULL
                    AND ts IS NOT NULL
                    AND serialized_bytes IS NOT NULL
            )
            WHERE rn = 1
            """
        )
        con.execute(
            """
            CREATE TABLE block_sync_states AS
            SELECT
                TRY_CAST(json_extract_string(json, '$.ts') AS BIGINT) AS ts,
                TRY_CAST(json_extract_string(json, '$.applying') AS BIGINT) AS applying,
                TRY_CAST(json_extract_string(json, '$.assigned_len') AS BIGINT) AS assigned_len,
                TRY_CAST(json_extract_string(json, '$.best_header_tip') AS BIGINT)
                    AS best_header_tip,
                TRY_CAST(json_extract_string(json, '$.body_download_floor') AS BIGINT)
                    AS body_download_floor,
                TRY_CAST(json_extract_string(json, '$.floor_gap_height') AS BIGINT)
                    AS floor_gap_height,
                json_extract_string(json, '$.floor_gap_state') AS floor_gap_state,
                TRY_CAST(json_extract_string(json, '$.floor_gap_servable_peers') AS BIGINT)
                    AS floor_gap_servable_peers,
                TRY_CAST(json_extract_string(json, '$.floor_gap_available_peers') AS BIGINT)
                    AS floor_gap_available_peers,
                TRY_CAST(json_extract_string(json, '$.floor_gap_outstanding_peers') AS BIGINT)
                    AS floor_gap_outstanding_peers,
                TRY_CAST(json_extract_string(json, '$.floor_gap_oldest_outstanding_ms') AS BIGINT)
                    AS floor_gap_oldest_outstanding_ms,
                TRY_CAST(json_extract_string(json, '$.floor_gap_next_deadline_ms') AS BIGINT)
                    AS floor_gap_next_deadline_ms,
                TRY_CAST(json_extract_string(json, '$.body_lag') AS BIGINT) AS body_lag,
                TRY_CAST(json_extract_string(json, '$.budget_available') AS BIGINT)
                    AS budget_available,
                TRY_CAST(json_extract_string(json, '$.budget_reserved') AS BIGINT)
                    AS budget_reserved,
                TRY_CAST(json_extract_string(json, '$.local_body_work') AS BIGINT)
                    AS local_body_work,
                TRY_CAST(json_extract_string(json, '$.needed_count') AS BIGINT) AS needed_count,
                TRY_CAST(json_extract_string(json, '$.outstanding') AS BIGINT) AS outstanding,
                TRY_CAST(json_extract_string(json, '$.peers') AS BIGINT) AS peers,
                TRY_CAST(json_extract_string(json, '$.peers_with_status') AS BIGINT)
                    AS peers_with_status,
                TRY_CAST(json_extract_string(json, '$.inbound_peers') AS BIGINT)
                    AS inbound_peers,
                TRY_CAST(json_extract_string(json, '$.outbound_peers') AS BIGINT)
                    AS outbound_peers,
                TRY_CAST(json_extract_string(json, '$.inbound_peers_with_status') AS BIGINT)
                    AS inbound_peers_with_status,
                TRY_CAST(json_extract_string(json, '$.outbound_peers_with_status') AS BIGINT)
                    AS outbound_peers_with_status,
                TRY_CAST(json_extract_string(json, '$.request_slot_capacity') AS BIGINT)
                    AS request_slot_capacity,
                TRY_CAST(json_extract_string(json, '$.request_slot_effective_window') AS BIGINT)
                    AS request_slot_effective_window,
                TRY_CAST(json_extract_string(json, '$.request_slot_available') AS BIGINT)
                    AS request_slot_available,
                TRY_CAST(json_extract_string(json, '$.request_slot_timeout_recovery') AS BIGINT)
                    AS request_slot_timeout_recovery,
                TRY_CAST(json_extract_string(json, '$.request_slot_saturated_peers') AS BIGINT)
                    AS request_slot_saturated_peers,
                TRY_CAST(json_extract_string(json, '$.queue_blocks') AS BIGINT) AS queue_blocks,
                TRY_CAST(json_extract_string(json, '$.queue_len') AS BIGINT) AS queue_len,
                TRY_CAST(json_extract_string(json, '$.refill_low_water') AS BIGINT)
                    AS refill_low_water,
                TRY_CAST(json_extract_string(json, '$.reorder') AS BIGINT) AS reorder,
                TRY_CAST(json_extract_string(json, '$.submitted_applies') AS BIGINT)
                    AS submitted_applies,
                TRY_CAST(json_extract_string(json, '$.verified_block_tip') AS BIGINT)
                    AS verified_block_tip
            FROM read_json_objects(?)
            WHERE json_extract_string(json, '$.event') = 'block_sync_state'
            """,
            [str(paths.block_sync)],
        )
        con.execute(
            """
            CREATE TABLE frontier_transitions AS
            SELECT
                TRY_CAST(json_extract_string(json, '$.ts') AS BIGINT) AS ts,
                TRY_CAST(json_extract_string(json, '$.sequence') AS BIGINT) AS sequence,
                json_extract_string(json, '$.result') AS result,
                json_extract_string(json, '$.cause') AS cause,
                TRY_CAST(json_extract_string(json, '$.new_finalized_height') AS BIGINT)
                    AS new_finalized_height,
                TRY_CAST(json_extract_string(json, '$.new_verified_body_height') AS BIGINT)
                    AS new_verified_body_height
            FROM read_json_objects(?)
            WHERE json_extract_string(json, '$.event') = 'sync_frontier_transition'
            """,
            [str(paths.commit_state)],
        )
        con.execute(
            """
            CREATE TABLE block_get_blocks_sent AS
            SELECT
                TRY_CAST(json_extract_string(json, '$.ts') AS BIGINT) AS ts,
                json_extract_string(json, '$.peer') AS peer,
                json_extract_string(json, '$.direction') AS direction,
                TRY_CAST(json_extract_string(json, '$.range_start') AS BIGINT) AS range_start,
                TRY_CAST(json_extract_string(json, '$.range_count') AS BIGINT) AS range_count,
                TRY_CAST(json_extract_string(json, '$.estimated_bytes') AS BIGINT)
                    AS estimated_bytes,
                TRY_CAST(json_extract_string(json, '$.available_slots') AS BIGINT)
                    AS available_slots,
                TRY_CAST(json_extract_string(json, '$.peer_outstanding') AS BIGINT)
                    AS peer_outstanding,
                TRY_CAST(json_extract_string(json, '$.hard_outbound_capacity') AS BIGINT)
                    AS hard_outbound_capacity,
                TRY_CAST(json_extract_string(json, '$.outbound_request_window') AS BIGINT)
                    AS outbound_request_window,
                TRY_CAST(json_extract_string(json, '$.timeout_recovery_slots') AS BIGINT)
                    AS timeout_recovery_slots
            FROM read_json_objects(?)
            WHERE json_extract_string(json, '$.event') = 'block_get_blocks_sent'
            """,
            [str(paths.block_sync)],
        )
        con.execute(
            """
            CREATE TABLE block_message_sent AS
            SELECT
                TRY_CAST(json_extract_string(json, '$.ts') AS BIGINT) AS ts,
                json_extract_string(json, '$.peer') AS peer,
                json_extract_string(json, '$.kind') AS kind,
                json_extract_string(json, '$.result') AS result,
                TRY_CAST(json_extract_string(json, '$.range_start') AS BIGINT) AS range_start,
                TRY_CAST(json_extract_string(json, '$.range_count') AS BIGINT) AS range_count,
                TRY_CAST(json_extract_string(json, '$.height') AS BIGINT) AS height,
                TRY_CAST(json_extract_string(json, '$.elapsed_ms') AS BIGINT) AS elapsed_ms
            FROM read_json_objects(?)
            WHERE json_extract_string(json, '$.event') = 'block_message_sent'
            """,
            [str(paths.block_sync)],
        )
        con.execute(
            """
            CREATE TABLE block_message_received AS
            SELECT
                TRY_CAST(json_extract_string(json, '$.ts') AS BIGINT) AS ts,
                json_extract_string(json, '$.peer') AS peer,
                json_extract_string(json, '$.kind') AS kind,
                TRY_CAST(json_extract_string(json, '$.range_start') AS BIGINT) AS range_start,
                TRY_CAST(json_extract_string(json, '$.range_count') AS BIGINT) AS range_count,
                TRY_CAST(json_extract_string(json, '$.height') AS BIGINT) AS height
            FROM read_json_objects(?)
            WHERE json_extract_string(json, '$.event') = 'block_message_received'
            """,
            [str(paths.block_sync)],
        )
        con.execute(
            """
            CREATE TABLE block_status_received AS
            SELECT
                TRY_CAST(json_extract_string(json, '$.ts') AS BIGINT) AS ts,
                json_extract_string(json, '$.peer') AS peer,
                TRY_CAST(json_extract_string(json, '$.range_start') AS BIGINT)
                    AS servable_low,
                TRY_CAST(json_extract_string(json, '$.height') AS BIGINT) AS servable_high,
                TRY_CAST(json_extract_string(json, '$.max_blocks_per_response') AS BIGINT)
                    AS max_blocks_per_response,
                TRY_CAST(json_extract_string(json, '$.max_inflight_requests') AS BIGINT)
                    AS max_inflight_requests,
                TRY_CAST(json_extract_string(json, '$.max_response_bytes') AS BIGINT)
                    AS max_response_bytes
            FROM read_json_objects(?)
            WHERE json_extract_string(json, '$.event') = 'block_status_received'
            """,
            [str(paths.block_sync)],
        )
        con.execute(
            """
            CREATE TABLE block_range_response_sent AS
            SELECT
                TRY_CAST(json_extract_string(json, '$.ts') AS BIGINT) AS ts,
                json_extract_string(json, '$.peer') AS peer,
                TRY_CAST(json_extract_string(json, '$.range_start') AS BIGINT) AS range_start,
                TRY_CAST(json_extract_string(json, '$.range_count') AS BIGINT) AS range_count,
                TRY_CAST(json_extract_string(json, '$.expected_count') AS BIGINT)
                    AS expected_count,
                TRY_CAST(json_extract_string(json, '$.serialized_bytes') AS BIGINT)
                    AS serialized_bytes,
                json_extract_string(json, '$.reason') AS reason,
                TRY_CAST(json_extract_string(json, '$.prepare_elapsed_ms') AS BIGINT)
                    AS prepare_elapsed_ms,
                TRY_CAST(json_extract_string(json, '$.send_elapsed_ms') AS BIGINT)
                    AS send_elapsed_ms,
                TRY_CAST(json_extract_string(json, '$.elapsed_ms') AS BIGINT) AS elapsed_ms
            FROM read_json_objects(?)
            WHERE json_extract_string(json, '$.event') = 'block_range_response_sent'
            """,
            [str(paths.block_sync)],
        )
        con.execute("CREATE TABLE cache_metadata(key VARCHAR PRIMARY KEY, metadata_json VARCHAR)")
        con.execute(
            "INSERT INTO cache_metadata VALUES ('trace_metadata', ?)",
            [json.dumps(metadata, sort_keys=True)],
        )
    finally:
        con.close()

    return db_path


def analyze_database(db_path: Path, out_dir: Path, bucket_seconds: int) -> dict[str, Any]:
    if bucket_seconds <= 0:
        raise ValueError("--bucket-seconds must be positive")

    out_dir.mkdir(parents=True, exist_ok=True)
    con = duckdb.connect(str(db_path), read_only=True)
    try:
        min_ts = fetch_min_ts(con)
        rates = build_rates(con, min_ts, bucket_seconds)
        states = build_state_intervals(con, min_ts)
        summary = build_summary(con, rates, states, min_ts, bucket_seconds, db_path)
    finally:
        con.close()

    rates.to_csv(out_dir / "rates.csv", index=False)
    states.to_csv(out_dir / "state_intervals.csv", index=False)
    write_summary(out_dir / "summary.md", summary)
    write_plots(rates, states, out_dir)

    return {
        "rates": rates,
        "state_intervals": states,
        "summary": summary,
    }


def fetch_min_ts(con: duckdb.DuckDBPyConnection) -> int:
    row = con.execute(
        """
        SELECT min(ts)
        FROM (
            SELECT ts FROM block_bodies WHERE ts IS NOT NULL
            UNION ALL
            SELECT ts FROM block_sync_states WHERE ts IS NOT NULL
            UNION ALL
            SELECT ts FROM frontier_transitions WHERE ts IS NOT NULL
        )
        """
    ).fetchone()
    if row is None or row[0] is None:
        raise ValueError("no timestamps found in cached trace tables")
    return int(row[0])


def bucket_expr(min_ts: int, bucket_seconds: int) -> str:
    bucket_us = bucket_seconds * 1_000_000
    return f"CAST(floor((ts - {min_ts}) / {bucket_us}) AS BIGINT) * {bucket_seconds}"


def build_rates(
    con: duckdb.DuckDBPyConnection,
    min_ts: int,
    bucket_seconds: int,
) -> pd.DataFrame:
    bucket_sql = bucket_expr(min_ts, bucket_seconds)
    raw_downloads = con.execute(
        f"""
        SELECT
            {bucket_sql} AS bucket_start_s,
            count(*) AS downloaded_raw_blocks,
            count(DISTINCT height) AS downloaded_distinct_heights,
            sum(coalesce(serialized_bytes, 0)) AS downloaded_raw_bytes
        FROM block_bodies
        WHERE ts IS NOT NULL
        GROUP BY 1
        ORDER BY 1
        """
    ).df()
    unique_downloads = con.execute(
        f"""
        SELECT
            {bucket_sql} AS bucket_start_s,
            count(*) AS downloaded_unique_blocks,
            sum(serialized_bytes) AS downloaded_unique_bytes
        FROM block_body_first_seen
        WHERE ts IS NOT NULL
        GROUP BY 1
        ORDER BY 1
        """
    ).df()
    finalized = finalized_rates(con, min_ts, bucket_seconds)

    rates = merge_rate_frames([raw_downloads, unique_downloads, finalized])
    count_columns = [
        "downloaded_raw_blocks",
        "downloaded_distinct_heights",
        "downloaded_raw_bytes",
        "downloaded_unique_blocks",
        "downloaded_unique_bytes",
        "finalized_blocks",
        "finalized_known_bytes",
        "finalized_missing_blocks",
    ]
    for column in count_columns:
        if column not in rates:
            rates[column] = 0
        rates[column] = rates[column].fillna(0).astype("int64")

    rates["downloaded_unique_blocks_per_s"] = (
        rates["downloaded_unique_blocks"] / bucket_seconds
    )
    rates["downloaded_raw_blocks_per_s"] = rates["downloaded_raw_blocks"] / bucket_seconds
    rates["downloaded_unique_bytes_per_s"] = (
        rates["downloaded_unique_bytes"] / bucket_seconds
    )
    rates["downloaded_raw_bytes_per_s"] = rates["downloaded_raw_bytes"] / bucket_seconds
    rates["finalized_blocks_per_s"] = rates["finalized_blocks"] / bucket_seconds
    rates["finalized_known_bytes_per_s"] = rates["finalized_known_bytes"] / bucket_seconds
    rates["cumulative_downloaded_unique_blocks"] = rates[
        "downloaded_unique_blocks"
    ].cumsum()
    rates["cumulative_finalized_blocks"] = rates["finalized_blocks"].cumsum()
    rates["downloaded_minus_finalized_blocks"] = (
        rates["cumulative_downloaded_unique_blocks"] - rates["cumulative_finalized_blocks"]
    )
    return rates.sort_values("bucket_start_s").reset_index(drop=True)


def finalized_rates(
    con: duckdb.DuckDBPyConnection,
    min_ts: int,
    bucket_seconds: int,
) -> pd.DataFrame:
    transitions = con.execute(
        """
        SELECT ts, sequence, new_finalized_height
        FROM frontier_transitions
        WHERE result = 'accepted'
            AND ts IS NOT NULL
            AND new_finalized_height IS NOT NULL
        ORDER BY ts, sequence NULLS LAST
        """
    ).df()
    if transitions.empty:
        return pd.DataFrame(
            columns=[
                "bucket_start_s",
                "finalized_blocks",
                "finalized_known_bytes",
                "finalized_missing_blocks",
            ]
        )

    body_bytes = con.execute(
        """
        SELECT height, serialized_bytes
        FROM block_body_first_seen
        WHERE height IS NOT NULL AND serialized_bytes IS NOT NULL
        ORDER BY height
        """
    ).df()
    heights = body_bytes["height"].to_numpy(dtype=np.int64)
    byte_cumsum = body_bytes["serialized_bytes"].to_numpy(dtype=np.int64).cumsum()

    rows: list[dict[str, int]] = []
    previous_height = 0
    monotonic_height = 0
    for row in transitions.itertuples(index=False):
        next_height = max(monotonic_height, int(row.new_finalized_height))
        delta = next_height - monotonic_height
        if delta <= 0:
            monotonic_height = next_height
            continue

        known_bytes, known_blocks = bytes_and_known_blocks_in_range(
            heights,
            byte_cumsum,
            previous_height + 1,
            next_height,
        )
        rows.append(
            {
                "bucket_start_s": int(
                    math.floor((int(row.ts) - min_ts) / (bucket_seconds * 1_000_000))
                    * bucket_seconds
                ),
                "finalized_blocks": delta,
                "finalized_known_bytes": known_bytes,
                "finalized_missing_blocks": delta - known_blocks,
            }
        )
        monotonic_height = next_height
        previous_height = next_height

    if not rows:
        return pd.DataFrame(
            columns=[
                "bucket_start_s",
                "finalized_blocks",
                "finalized_known_bytes",
                "finalized_missing_blocks",
            ]
        )

    return (
        pd.DataFrame(rows)
        .groupby("bucket_start_s", as_index=False)
        .sum(numeric_only=True)
        .sort_values("bucket_start_s")
    )


def bytes_and_known_blocks_in_range(
    heights: np.ndarray,
    byte_cumsum: np.ndarray,
    start_height: int,
    end_height: int,
) -> tuple[int, int]:
    if heights.size == 0 or end_height < start_height:
        return 0, 0

    start_index = int(np.searchsorted(heights, start_height, side="left"))
    end_index = int(np.searchsorted(heights, end_height, side="right"))
    known_blocks = end_index - start_index
    if known_blocks <= 0:
        return 0, 0

    before = int(byte_cumsum[start_index - 1]) if start_index > 0 else 0
    known_bytes = int(byte_cumsum[end_index - 1]) - before
    return known_bytes, known_blocks


def merge_rate_frames(frames: list[pd.DataFrame]) -> pd.DataFrame:
    non_empty = [frame for frame in frames if not frame.empty]
    if not non_empty:
        return pd.DataFrame({"bucket_start_s": []})

    merged = non_empty[0]
    for frame in non_empty[1:]:
        merged = merged.merge(frame, on="bucket_start_s", how="outer")
    return merged.sort_values("bucket_start_s").reset_index(drop=True)


def build_state_intervals(con: duckdb.DuckDBPyConnection, min_ts: int) -> pd.DataFrame:
    states = con.execute(
        """
        SELECT *
        FROM block_sync_states
        WHERE ts IS NOT NULL
        ORDER BY ts
        """
    ).df()
    if states.empty:
        return pd.DataFrame(
            columns=[
                "start_s",
                "end_s",
                "duration_s",
                "blocking_class",
            ]
        )

    numeric_columns = [
        column for column in states.columns if column not in ("ts", "floor_gap_state")
    ]
    states[numeric_columns] = states[numeric_columns].fillna(0)
    states["floor_gap_state"] = states["floor_gap_state"].fillna("unknown")
    states["next_ts"] = states["ts"].shift(-1)
    states["next_body_download_floor"] = states["body_download_floor"].shift(-1)
    states["start_s"] = (states["ts"] - min_ts) / 1_000_000
    states["end_s"] = (states["next_ts"] - min_ts) / 1_000_000
    states["duration_s"] = ((states["next_ts"] - states["ts"]) / 1_000_000).clip(lower=0)
    states["download_budget_total"] = states["budget_reserved"] + states["budget_available"]
    states["download_budget_used_pct"] = np.where(
        states["download_budget_total"] > 0,
        states["budget_reserved"] / states["download_budget_total"] * 100.0,
        0.0,
    )
    states["download_budget_saturated"] = (
        (states["budget_available"] == 0) | (states["download_budget_used_pct"] >= 95.0)
    )
    states["request_slot_used_pct"] = np.where(
        states["request_slot_capacity"] > 0,
        states["outstanding"] / states["request_slot_capacity"] * 100.0,
        0.0,
    )
    states["request_window_used_pct"] = np.where(
        states["request_slot_effective_window"] > 0,
        states["outstanding"] / states["request_slot_effective_window"] * 100.0,
        0.0,
    )
    states["request_slots_saturated"] = (
        (states["request_slot_capacity"] > 0) & (states["request_slot_used_pct"] >= 95.0)
    )
    states["hol_gap_active"] = states.apply(is_hol_gap_interval, axis=1)
    states["hol_gap_reorder_blocks"] = states["reorder"].where(states["hol_gap_active"], 0)
    states["blocking_class"] = states.apply(classify_state_interval, axis=1)
    states["download_floor_minus_verified"] = (
        states["body_download_floor"] - states["verified_block_tip"]
    )

    ordered_columns = [
        "start_s",
        "end_s",
        "duration_s",
        "blocking_class",
        "best_header_tip",
        "body_download_floor",
        "floor_gap_height",
        "floor_gap_state",
        "floor_gap_servable_peers",
        "floor_gap_available_peers",
        "floor_gap_outstanding_peers",
        "floor_gap_oldest_outstanding_ms",
        "floor_gap_next_deadline_ms",
        "verified_block_tip",
        "download_floor_minus_verified",
        "body_lag",
        "reorder",
        "hol_gap_active",
        "hol_gap_reorder_blocks",
        "applying",
        "submitted_applies",
        "local_body_work",
        "refill_low_water",
        "peers",
        "peers_with_status",
        "inbound_peers",
        "outbound_peers",
        "inbound_peers_with_status",
        "outbound_peers_with_status",
        "budget_reserved",
        "budget_available",
        "download_budget_used_pct",
        "download_budget_saturated",
        "assigned_len",
        "outstanding",
        "request_slot_capacity",
        "request_slot_effective_window",
        "request_slot_available",
        "request_slot_timeout_recovery",
        "request_slot_saturated_peers",
        "request_slot_used_pct",
        "request_window_used_pct",
        "request_slots_saturated",
        "queue_blocks",
        "queue_len",
        "needed_count",
    ]
    return states[ordered_columns].iloc[:-1].reset_index(drop=True)


def is_hol_gap_interval(row: pd.Series) -> bool:
    floor_stalled = (
        not pd.isna(row.next_body_download_floor)
        and int(row.next_body_download_floor) <= int(row.body_download_floor)
    )
    return int(row.reorder) > 0 and floor_stalled and int(row.body_lag) > 0


def classify_state_interval(row: pd.Series) -> str:
    body_lag = int(row.body_lag)
    budget_available = int(row.budget_available)
    budget_reserved = int(row.budget_reserved)
    total_budget = budget_available + budget_reserved
    budget_near_max = total_budget > 0 and (budget_reserved / total_budget) >= 0.95

    if body_lag == 0:
        return "header_limited"
    if body_lag > 0 and int(row.peers_with_status) == 0:
        return "peer_unavailable"
    if is_hol_gap_interval(row):
        return "hol_gap"
    if (
        int(row.verified_block_tip) < int(row.body_download_floor)
        or int(row.applying) > 0
        or int(row.submitted_applies) > 0
    ):
        return "commit_backpressure"
    if (
        body_lag > 0
        and int(row.local_body_work) < int(row.refill_low_water)
        and int(row.peers_with_status) > 0
        and not budget_near_max
        and budget_available > 0
    ):
        return "download_starved"
    return "scheduler_idle_or_unknown"


def build_summary(
    con: duckdb.DuckDBPyConnection,
    rates: pd.DataFrame,
    states: pd.DataFrame,
    min_ts: int,
    bucket_seconds: int,
    db_path: Path,
) -> dict[str, Any]:
    counts = con.execute(
        """
        SELECT
            (SELECT count(*) FROM block_bodies) AS raw_body_rows,
            (SELECT count(*) FROM block_body_first_seen) AS unique_body_heights,
            (SELECT count(*) FROM block_sync_states) AS state_rows,
            (SELECT count(*) FROM frontier_transitions) AS frontier_rows,
            (SELECT count(*) FROM frontier_transitions WHERE result = 'accepted')
                AS accepted_frontier_rows,
            (SELECT count(*) FROM block_message_sent) AS block_message_sent_rows,
            (SELECT count(*) FROM block_message_received) AS block_message_received_rows,
            (SELECT count(*) FROM block_range_response_sent)
                AS block_range_response_sent_rows,
            (SELECT count(*) FROM block_message_sent WHERE result != 'queued')
                AS block_message_send_failures,
            (SELECT count(*) FROM block_bodies
                WHERE ts IS NULL OR height IS NULL OR serialized_bytes IS NULL)
                AS body_rows_with_missing_required_fields,
            (SELECT count(*) FROM block_sync_states
                WHERE ts IS NULL OR body_lag IS NULL OR body_download_floor IS NULL
                    OR verified_block_tip IS NULL)
                AS state_rows_with_missing_required_fields,
            (SELECT count(*) FROM frontier_transitions
                WHERE ts IS NULL OR new_finalized_height IS NULL)
                AS frontier_rows_with_missing_required_fields
        """
    ).df().iloc[0].to_dict()

    if rates.empty:
        duration_s = 0.0
    else:
        duration_s = float(rates["bucket_start_s"].max() + bucket_seconds)

    blocking = blocking_breakdown(states)
    peak_download_finalized_backlog = (
        int(rates["downloaded_minus_finalized_blocks"].max()) if not rates.empty else 0
    )
    peak_floor_verified_backlog = (
        int(states["download_floor_minus_verified"].max()) if not states.empty else 0
    )
    hol_gap_duration_s = (
        float(states.loc[states["hol_gap_active"], "duration_s"].sum())
        if not states.empty
        else 0.0
    )
    budget_saturated_duration_s = (
        float(states.loc[states["download_budget_saturated"], "duration_s"].sum())
        if not states.empty
        else 0.0
    )
    request_slots_saturated_duration_s = (
        float(states.loc[states["request_slots_saturated"], "duration_s"].sum())
        if not states.empty
        else 0.0
    )
    if states.empty or states["duration_s"].sum() <= 0:
        weighted_budget_used_pct = 0.0
        weighted_request_slot_used_pct = 0.0
        weighted_request_window_used_pct = 0.0
    else:
        weighted_budget_used_pct = float(
            (states["download_budget_used_pct"] * states["duration_s"]).sum()
            / states["duration_s"].sum()
        )
        weighted_request_slot_used_pct = float(
            (states["request_slot_used_pct"] * states["duration_s"]).sum()
            / states["duration_s"].sum()
        )
        weighted_request_window_used_pct = float(
            (states["request_window_used_pct"] * states["duration_s"]).sum()
            / states["duration_s"].sum()
        )

    totals = {
        "downloaded_unique_blocks": int(rates["downloaded_unique_blocks"].sum())
        if not rates.empty
        else 0,
        "downloaded_raw_blocks": int(rates["downloaded_raw_blocks"].sum())
        if not rates.empty
        else 0,
        "downloaded_unique_bytes": int(rates["downloaded_unique_bytes"].sum())
        if not rates.empty
        else 0,
        "downloaded_raw_bytes": int(rates["downloaded_raw_bytes"].sum())
        if not rates.empty
        else 0,
        "finalized_blocks": int(rates["finalized_blocks"].sum()) if not rates.empty else 0,
        "finalized_known_bytes": int(rates["finalized_known_bytes"].sum())
        if not rates.empty
        else 0,
        "finalized_missing_blocks": int(rates["finalized_missing_blocks"].sum())
        if not rates.empty
        else 0,
    }

    return {
        "cache_db": str(db_path),
        "min_ts": min_ts,
        "bucket_seconds": bucket_seconds,
        "duration_s": duration_s,
        "counts": {key: int(value) for key, value in counts.items()},
        "totals": totals,
        "average_rates": {
            key: (value / duration_s if duration_s > 0 else 0.0)
            for key, value in totals.items()
        },
        "peak_download_finalized_backlog": peak_download_finalized_backlog,
        "peak_floor_verified_backlog": peak_floor_verified_backlog,
        "hol_gap_duration_s": hol_gap_duration_s,
        "download_budget_saturated_duration_s": budget_saturated_duration_s,
        "weighted_download_budget_used_pct": weighted_budget_used_pct,
        "request_slots_saturated_duration_s": request_slots_saturated_duration_s,
        "weighted_request_slot_used_pct": weighted_request_slot_used_pct,
        "weighted_request_window_used_pct": weighted_request_window_used_pct,
        "peak_request_slot_capacity": int(states["request_slot_capacity"].max())
        if not states.empty
        else 0,
        "peak_request_slot_available": int(states["request_slot_available"].max())
        if not states.empty
        else 0,
        "peak_request_slot_used_pct": float(states["request_slot_used_pct"].max())
        if not states.empty
        else 0.0,
        "peak_request_window_used_pct": float(states["request_window_used_pct"].max())
        if not states.empty
        else 0.0,
        "blocking_breakdown": blocking,
        "hol_gap_diagnostics": hol_gap_diagnostics(states),
        "status_cap_diagnostics": status_cap_diagnostics(con),
        "get_blocks_sent_diagnostics": get_blocks_sent_diagnostics(con),
        "message_diagnostics": message_diagnostics(con),
    }


def blocking_breakdown(states: pd.DataFrame) -> list[dict[str, Any]]:
    if states.empty:
        return []
    grouped = (
        states.groupby("blocking_class", as_index=False)["duration_s"]
        .sum()
        .sort_values("duration_s", ascending=False)
    )
    total_duration = float(grouped["duration_s"].sum())
    grouped["percent"] = (
        grouped["duration_s"] / total_duration * 100.0 if total_duration > 0 else 0.0
    )
    return grouped.to_dict(orient="records")


def hol_gap_diagnostics(states: pd.DataFrame) -> list[dict[str, Any]]:
    if states.empty or "floor_gap_state" not in states.columns:
        return []

    hol = states.loc[states["hol_gap_active"]].copy()
    if hol.empty:
        return []

    grouped = (
        hol.groupby("floor_gap_state", as_index=False)
        .agg(
            duration_s=("duration_s", "sum"),
            avg_servable_peers=("floor_gap_servable_peers", "mean"),
            avg_available_peers=("floor_gap_available_peers", "mean"),
            avg_outstanding_peers=("floor_gap_outstanding_peers", "mean"),
            max_oldest_outstanding_ms=("floor_gap_oldest_outstanding_ms", "max"),
            min_next_deadline_ms=("floor_gap_next_deadline_ms", "min"),
        )
        .sort_values("duration_s", ascending=False)
    )
    total_duration = float(grouped["duration_s"].sum())
    grouped["percent"] = (
        grouped["duration_s"] / total_duration * 100.0 if total_duration > 0 else 0.0
    )
    return grouped.to_dict(orient="records")


def status_cap_diagnostics(con: duckdb.DuckDBPyConnection) -> list[dict[str, Any]]:
    try:
        caps = con.execute(
            """
            SELECT
                max_blocks_per_response,
                max_inflight_requests,
                max_response_bytes,
                count(*) AS count,
                count(DISTINCT peer) AS peers
            FROM block_status_received
            GROUP BY
                max_blocks_per_response,
                max_inflight_requests,
                max_response_bytes
            ORDER BY count DESC
            """
        ).df()
    except duckdb.CatalogException:
        return []
    return caps.to_dict(orient="records")


def get_blocks_sent_diagnostics(con: duckdb.DuckDBPyConnection) -> list[dict[str, Any]]:
    try:
        diagnostics = con.execute(
            """
            SELECT
                coalesce(direction, 'unknown') AS direction,
                count(*) AS count,
                count(DISTINCT peer) AS peers,
                avg(range_count) AS avg_count,
                avg(estimated_bytes) AS avg_estimated_bytes,
                avg(available_slots) AS avg_available_slots_before_send,
                max(available_slots) AS max_available_slots_before_send,
                avg(peer_outstanding) AS avg_peer_outstanding_before_send,
                max(peer_outstanding) AS max_peer_outstanding_before_send,
                max(hard_outbound_capacity) AS max_hard_outbound_capacity,
                max(outbound_request_window) AS max_outbound_request_window
            FROM block_get_blocks_sent
            GROUP BY coalesce(direction, 'unknown')
            ORDER BY count DESC, direction
            """
        ).df()
    except duckdb.CatalogException:
        return []
    return diagnostics.to_dict(orient="records")


def message_diagnostics(con: duckdb.DuckDBPyConnection) -> dict[str, list[dict[str, Any]]]:
    sent_by_kind_result = con.execute(
        """
        SELECT kind, result, count(*) AS count
        FROM block_message_sent
        GROUP BY kind, result
        ORDER BY count DESC, kind, result
        """
    ).df()
    received_by_kind = con.execute(
        """
        SELECT kind, count(*) AS count
        FROM block_message_received
        GROUP BY kind
        ORDER BY count DESC, kind
        """
    ).df()
    response_latency = con.execute(
        """
        SELECT
            reason,
            count(*) AS count,
            avg(range_count) AS avg_returned,
            avg(expected_count) AS avg_expected,
            avg(serialized_bytes) AS avg_serialized_bytes,
            quantile_cont(prepare_elapsed_ms, 0.50) AS prepare_p50_ms,
            quantile_cont(prepare_elapsed_ms, 0.95) AS prepare_p95_ms,
            max(prepare_elapsed_ms) AS prepare_max_ms,
            quantile_cont(send_elapsed_ms, 0.50) AS send_p50_ms,
            quantile_cont(send_elapsed_ms, 0.95) AS send_p95_ms,
            max(send_elapsed_ms) AS send_max_ms,
            quantile_cont(elapsed_ms, 0.50) AS total_p50_ms,
            quantile_cont(elapsed_ms, 0.95) AS total_p95_ms,
            max(elapsed_ms) AS total_max_ms
        FROM block_range_response_sent
        GROUP BY reason
        ORDER BY count DESC, reason
        """
    ).df()
    return {
        "sent_by_kind_result": sent_by_kind_result.to_dict(orient="records"),
        "received_by_kind": received_by_kind.to_dict(orient="records"),
        "response_latency": response_latency.to_dict(orient="records"),
    }


def write_summary(path: Path, summary: dict[str, Any]) -> None:
    counts = summary["counts"]
    totals = summary["totals"]
    averages = summary["average_rates"]
    blocking = summary["blocking_breakdown"]
    hol_diagnostics = summary["hol_gap_diagnostics"]
    status_caps = summary["status_cap_diagnostics"]
    get_blocks_sent = summary["get_blocks_sent_diagnostics"]
    diagnostics = summary["message_diagnostics"]
    slot_trace_available = summary["peak_request_slot_capacity"] > 0
    slot_weighted_pct = (
        f"{summary['weighted_request_slot_used_pct']:.2f}%"
        if slot_trace_available
        else "n/a"
    )
    window_weighted_pct = (
        f"{summary['weighted_request_window_used_pct']:.2f}%"
        if slot_trace_available
        else "n/a"
    )
    slot_saturated_duration = (
        f"{summary['request_slots_saturated_duration_s']:.3f} seconds"
        if slot_trace_available
        else "n/a"
    )
    peak_slot_pct = (
        f"{summary['peak_request_slot_used_pct']:.2f}%"
        if slot_trace_available
        else "n/a"
    )
    peak_window_pct = (
        f"{summary['peak_request_window_used_pct']:.2f}%"
        if slot_trace_available
        else "n/a"
    )
    peak_slot_capacity = (
        f"{summary['peak_request_slot_capacity']:,}" if slot_trace_available else "n/a"
    )
    peak_available_slots = (
        f"{summary['peak_request_slot_available']:,}" if slot_trace_available else "n/a"
    )

    lines = [
        "# Zakura Sync Throughput Summary",
        "",
        f"- Cache database: `{summary['cache_db']}`",
        f"- Bucket size: {summary['bucket_seconds']} seconds",
        f"- Trace start timestamp: {summary['min_ts']} us",
        f"- Approximate analyzed duration: {summary['duration_s']:.1f} seconds",
        "",
        "## Event Counts",
        "",
        f"- Raw block body rows: {counts['raw_body_rows']:,}",
        f"- Unique downloaded heights: {counts['unique_body_heights']:,}",
        f"- Block sync state samples: {counts['state_rows']:,}",
        f"- Frontier transitions: {counts['frontier_rows']:,}",
        f"- Accepted frontier transitions: {counts['accepted_frontier_rows']:,}",
        f"- Block message sent rows: {counts['block_message_sent_rows']:,}",
        f"- Block message received rows: {counts['block_message_received_rows']:,}",
        f"- Block range response sent rows: {counts['block_range_response_sent_rows']:,}",
        f"- Block message send failures/timeouts: {counts['block_message_send_failures']:,}",
        "",
        "## Throughput Totals",
        "",
        f"- Downloaded unique blocks: {totals['downloaded_unique_blocks']:,}",
        f"- Downloaded raw body rows: {totals['downloaded_raw_blocks']:,}",
        f"- Downloaded unique bytes: {totals['downloaded_unique_bytes']:,}",
        f"- Downloaded raw bytes: {totals['downloaded_raw_bytes']:,}",
        f"- Finalized blocks: {totals['finalized_blocks']:,}",
        f"- Finalized known bytes: {totals['finalized_known_bytes']:,}",
        f"- Finalized blocks missing body-byte rows: {totals['finalized_missing_blocks']:,}",
        "",
        "## Average Rates",
        "",
        f"- Downloaded unique blocks/s: {averages['downloaded_unique_blocks']:.3f}",
        f"- Downloaded raw blocks/s: {averages['downloaded_raw_blocks']:.3f}",
        f"- Downloaded unique MiB/s: {averages['downloaded_unique_bytes'] / 1048576:.3f}",
        f"- Downloaded raw MiB/s: {averages['downloaded_raw_bytes'] / 1048576:.3f}",
        f"- Finalized blocks/s: {averages['finalized_blocks']:.3f}",
        f"- Finalized known MiB/s: {averages['finalized_known_bytes'] / 1048576:.3f}",
        "",
        "## Backlog Peaks",
        "",
        "- Downloaded-minus-finalized blocks: "
        f"{summary['peak_download_finalized_backlog']:,}",
        "- Body-download-floor-minus-verified-tip blocks: "
        f"{summary['peak_floor_verified_backlog']:,}",
        f"- HoL gap active time: {summary['hol_gap_duration_s']:.3f} seconds",
        "",
        "## Download Budget Utilization",
        "",
        "- Wall-clock weighted reserved budget: "
        f"{summary['weighted_download_budget_used_pct']:.2f}%",
        "- Time with reserved download budget >= 95%: "
        f"{summary['download_budget_saturated_duration_s']:.3f} seconds",
        "",
        "## Request Slot Utilization",
        "",
        f"- Wall-clock weighted hard slot utilization: {slot_weighted_pct}",
        f"- Wall-clock weighted adaptive-window utilization: {window_weighted_pct}",
        f"- Time with hard request slots >= 95% full: {slot_saturated_duration}",
        f"- Peak hard request slot utilization: {peak_slot_pct}",
        f"- Peak adaptive-window utilization: {peak_window_pct}",
        f"- Peak hard request slot capacity: {peak_slot_capacity}",
        f"- Peak available request slots: {peak_available_slots}",
        "",
        "## Blocking Breakdown",
        "",
        "| Class | Seconds | Percent |",
        "| --- | ---: | ---: |",
    ]
    for row in blocking:
        lines.append(
            f"| {row['blocking_class']} | {row['duration_s']:.3f} | {row['percent']:.2f}% |"
        )
    lines.extend(
        [
            "",
            "## HoL Gap Diagnostics",
            "",
            "| Floor State | Seconds | Percent | Avg Servable Peers | Avg Available Peers | Avg Outstanding Peers | Oldest Outstanding Max ms | Next Deadline Min ms |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for row in hol_diagnostics:
        lines.append(
            "| "
            f"{_display(row['floor_gap_state'])} | "
            f"{row['duration_s']:.3f} | "
            f"{row['percent']:.2f}% | "
            f"{_format_number(row['avg_servable_peers'])} | "
            f"{_format_number(row['avg_available_peers'])} | "
            f"{_format_number(row['avg_outstanding_peers'])} | "
            f"{_format_number(row['max_oldest_outstanding_ms'])} | "
            f"{_format_number(row['min_next_deadline_ms'])} |"
        )
    if not hol_diagnostics:
        lines.append("| n/a | 0 | 0.00% | n/a | n/a | n/a | n/a | n/a |")
    lines.extend(
        [
            "",
            "## Peer Status Caps",
            "",
            "| Max Blocks | Max In-Flight | Max Response MiB | Rows | Peers |",
            "| ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for row in status_caps:
        lines.append(
            "| "
            f"{_display(row['max_blocks_per_response'])} | "
            f"{_display(row['max_inflight_requests'])} | "
            f"{_format_mib(row['max_response_bytes'])} | "
            f"{row['count']:,} | "
            f"{row['peers']:,} |"
        )
    if not status_caps:
        lines.append("| n/a | n/a | n/a | 0 | 0 |")
    lines.extend(
        [
            "",
            "## GetBlocks Send Diagnostics",
            "",
            "| Direction | Sends | Peers | Avg Count | Avg Est MiB | Avg Slots Before | Max Slots Before | Avg Peer Out Before | Max Peer Out Before | Max Hard Cap | Max Window |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for row in get_blocks_sent:
        lines.append(
            "| "
            f"{_display(row['direction'])} | "
            f"{row['count']:,} | "
            f"{row['peers']:,} | "
            f"{_format_number(row['avg_count'])} | "
            f"{_format_mib(row['avg_estimated_bytes'])} | "
            f"{_format_number(row['avg_available_slots_before_send'])} | "
            f"{_display(row['max_available_slots_before_send'])} | "
            f"{_format_number(row['avg_peer_outstanding_before_send'])} | "
            f"{_display(row['max_peer_outstanding_before_send'])} | "
            f"{_display(row['max_hard_outbound_capacity'])} | "
            f"{_display(row['max_outbound_request_window'])} |"
        )
    if not get_blocks_sent:
        lines.append("| n/a | 0 | 0 | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a |")
    lines.extend(
        [
            "",
            "## Block Message Diagnostics",
            "",
            "### Sent Messages",
            "",
            "| Kind | Result | Count |",
            "| --- | --- | ---: |",
        ]
    )
    for row in diagnostics["sent_by_kind_result"]:
        lines.append(f"| {_display(row['kind'])} | {_display(row['result'])} | {row['count']:,} |")
    if not diagnostics["sent_by_kind_result"]:
        lines.append("| n/a | n/a | 0 |")
    lines.extend(
        [
            "",
            "### Received Messages",
            "",
            "| Kind | Count |",
            "| --- | ---: |",
        ]
    )
    for row in diagnostics["received_by_kind"]:
        lines.append(f"| {_display(row['kind'])} | {row['count']:,} |")
    if not diagnostics["received_by_kind"]:
        lines.append("| n/a | 0 |")
    lines.extend(
        [
            "",
            "### Served Range Latency",
            "",
            "| Reason | Count | Avg Returned | Avg Expected | Avg MiB | Prepare p50/p95/max ms | Send p50/p95/max ms | Total p50/p95/max ms |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for row in diagnostics["response_latency"]:
        lines.append(
            "| "
            f"{_display(row['reason'])} | "
            f"{row['count']:,} | "
            f"{_format_number(row['avg_returned'])} | "
            f"{_format_number(row['avg_expected'])} | "
            f"{_format_mib(row['avg_serialized_bytes'])} | "
            f"{_format_triplet(row, 'prepare')} | "
            f"{_format_triplet(row, 'send')} | "
            f"{_format_triplet(row, 'total')} |"
        )
    if not diagnostics["response_latency"]:
        lines.append("| n/a | 0 | n/a | n/a | n/a | n/a | n/a | n/a |")
    lines.extend(
        [
            "",
            "## Missing Trace Fields",
            "",
            "- Block body rows missing ts, height, or serialized_bytes: "
            f"{counts['body_rows_with_missing_required_fields']:,}",
            "- Block sync state rows missing core backlog fields: "
            f"{counts['state_rows_with_missing_required_fields']:,}",
            "- Frontier transition rows missing ts or finalized height: "
            f"{counts['frontier_rows_with_missing_required_fields']:,}",
            "",
            "Blocking classes are approximate because block_sync_state is sampled periodically.",
            "`sync_backlog.png` focuses on bottleneck-oriented raw signals; the broader "
            "downloaded-minus-finalized backlog is still available in `rates.csv`.",
            "",
        ]
    )
    path.write_text("\n".join(lines), encoding="utf-8")


def _display(value: Any) -> str:
    if value is None or (isinstance(value, float) and math.isnan(value)):
        return "n/a"
    return str(value)


def _format_number(value: Any) -> str:
    if value is None or (isinstance(value, float) and math.isnan(value)):
        return "n/a"
    return f"{float(value):.1f}"


def _format_mib(value: Any) -> str:
    if value is None or (isinstance(value, float) and math.isnan(value)):
        return "n/a"
    return f"{float(value) / 1048576:.3f}"


def _format_triplet(row: dict[str, Any], prefix: str) -> str:
    values = [
        row[f"{prefix}_p50_ms"],
        row[f"{prefix}_p95_ms"],
        row[f"{prefix}_max_ms"],
    ]
    if all(value is None or (isinstance(value, float) and math.isnan(value)) for value in values):
        return "n/a"
    return "/".join(_format_number(value) for value in values)


def write_plots(rates: pd.DataFrame, states: pd.DataFrame, out_dir: Path) -> None:
    write_throughput_plot(rates, out_dir / "sync_throughput.png")
    write_backlog_plot(states, out_dir / "sync_backlog.png")
    write_bottleneck_timeline_plot(states, out_dir / "bottleneck_timeline.png")
    write_budget_utilization_plot(states, out_dir / "download_budget_utilization.png")
    write_blocking_plot(states, out_dir / "blocking_breakdown.png")


def write_throughput_plot(rates: pd.DataFrame, path: Path) -> None:
    fig, axes = plt.subplots(2, 1, figsize=(12, 8), sharex=True)
    # Rate-line handles and running-average handles per axis. The averages get
    # their own legend so the main legend stays limited to the rate series.
    line_handles: list[list[plt.Line2D]] = [[], []]
    avg_handles: list[list[plt.Line2D]] = [[], []]
    if not rates.empty:
        x = rates["bucket_start_s"] / 60.0
        panels = (
            (
                axes[0],
                line_handles[0],
                avg_handles[0],
                "blocks/s",
                (
                    (rates["downloaded_raw_blocks_per_s"], "downloaded raw", "#ff7f0e", 0.65),
                    (rates["finalized_blocks_per_s"], "finalized", "#2ca02c", 1.0),
                ),
            ),
            (
                axes[1],
                line_handles[1],
                avg_handles[1],
                "MiB/s",
                (
                    (rates["downloaded_raw_bytes_per_s"] / 1048576, "downloaded raw", "#ff7f0e", 0.65),
                    (rates["finalized_known_bytes_per_s"] / 1048576, "finalized known", "#2ca02c", 1.0),
                ),
            ),
        )
        for axis, lines, avgs, unit, series in panels:
            for values, label, color, alpha in series:
                line, avg = plot_throughput_series(
                    axis, x, values, label, color, unit, line_alpha=alpha
                )
                lines.append(line)
                avgs.append(avg)
    axes[0].set_ylabel("blocks/s")
    axes[1].set_ylabel("MiB/s")
    axes[1].set_xlabel("minutes since trace start")
    for lines, avgs, axis in zip(line_handles, avg_handles, axes):
        axis.grid(True, alpha=0.25)
        if lines:
            main_legend = axis.legend(
                lines, [h.get_label() for h in lines], loc="upper left"
            )
            # Keep the main legend when a second legend is added below.
            axis.add_artist(main_legend)
        if avgs:
            axis.legend(
                avgs,
                [h.get_label() for h in avgs],
                loc="upper right",
                title="averages",
            )
    fig.suptitle("Zakura Sync Throughput")
    fig.tight_layout()
    fig.savefig(path, dpi=150)
    plt.close(fig)


def plot_throughput_series(
    axis: plt.Axes,
    x: pd.Series,
    values: pd.Series,
    label: str,
    color: str,
    unit: str,
    *,
    line_alpha: float = 1.0,
) -> tuple[plt.Line2D, plt.Line2D]:
    (line,) = axis.plot(x, values, label=label, color=color, alpha=line_alpha)

    mean = float(values.mean())
    stdev = float(values.std(ddof=0))
    avg = axis.axhline(
        mean,
        color=color,
        linestyle="--",
        linewidth=1.2,
        alpha=0.75,
        label=f"{label}: {mean:.2f} +/- {stdev:.2f} {unit}",
    )
    axis.axhspan(
        max(0.0, mean - stdev),
        mean + stdev,
        color=color,
        alpha=0.06,
        linewidth=0,
    )
    return line, avg


def write_backlog_plot(states: pd.DataFrame, path: Path) -> None:
    fig, axis = plt.subplots(figsize=(12, 6))
    if not states.empty:
        x = states["start_s"] / 60.0
        axis.plot(
            x,
            states["download_floor_minus_verified"],
            label="commit backlog: floor - verified",
            linewidth=2,
        )
        axis.plot(x, states["reorder"], label="reorder buffer", alpha=0.85)
        axis.plot(
            x,
            states["submitted_applies"],
            label="apply queue: submitted applies",
            alpha=0.85,
        )
        axis.plot(
            x,
            states["hol_gap_reorder_blocks"],
            label="HoL gap: stalled reorder blocks",
            color="#9467bd",
            linewidth=2,
            alpha=0.95,
        )
        for row in states.loc[states["hol_gap_active"]].itertuples(index=False):
            axis.axvspan(
                float(row.start_s) / 60.0,
                float(row.end_s) / 60.0,
                color="#9467bd",
                alpha=0.08,
                linewidth=0,
            )
    axis.set_title("Zakura Bottleneck Backlog Signals")
    axis.set_xlabel("minutes since trace start")
    axis.set_ylabel("blocks")
    axis.grid(True, alpha=0.25)
    if not states.empty:
        axis.legend(loc="best")
    fig.tight_layout()
    fig.savefig(path, dpi=150)
    plt.close(fig)


def write_bottleneck_timeline_plot(states: pd.DataFrame, path: Path) -> None:
    class_order = [
        "commit_backpressure",
        "hol_gap",
        "download_starved",
        "peer_unavailable",
        "header_limited",
        "scheduler_idle_or_unknown",
    ]
    class_colors = {
        "commit_backpressure": "#1f77b4",
        "hol_gap": "#9467bd",
        "download_starved": "#ff7f0e",
        "peer_unavailable": "#8c564b",
        "header_limited": "#2ca02c",
        "scheduler_idle_or_unknown": "#7f7f7f",
    }

    fig, axis = plt.subplots(figsize=(12, 4.8))
    if not states.empty:
        present_classes = [
            class_name
            for class_name in class_order
            if class_name in set(states["blocking_class"])
        ]
        y_positions = {class_name: index for index, class_name in enumerate(present_classes)}
        for row in states.itertuples(index=False):
            duration_min = float(row.duration_s) / 60.0
            if duration_min <= 0:
                continue
            y = y_positions[row.blocking_class]
            axis.broken_barh(
                [(float(row.start_s) / 60.0, duration_min)],
                (y - 0.4, 0.8),
                facecolors=class_colors.get(row.blocking_class, "#7f7f7f"),
            )
        axis.set_yticks(list(y_positions.values()), list(y_positions.keys()))
        axis.set_ylim(-0.6, len(present_classes) - 0.4)
        axis.set_xlim(
            0,
            max(float(states["end_s"].max()) / 60.0, float(states["start_s"].max()) / 60.0),
        )
    axis.set_title("Primary Bottleneck Over Time")
    axis.set_xlabel("minutes since trace start")
    axis.grid(True, axis="x", alpha=0.25)
    fig.tight_layout()
    fig.savefig(path, dpi=150)
    plt.close(fig)


def write_budget_utilization_plot(states: pd.DataFrame, path: Path) -> None:
    fig, axes = plt.subplots(2, 1, figsize=(12, 7), sharex=True)
    if not states.empty:
        x = states["start_s"] / 60.0
        axes[0].plot(
            x,
            states["download_budget_used_pct"],
            label="reserved download budget",
            color="#d62728",
            linewidth=1.8,
        )
        axes[0].axhline(95.0, color="#7f7f7f", linestyle="--", linewidth=1, label="95%")
        axes[0].fill_between(
            x,
            0,
            states["download_budget_used_pct"],
            where=states["download_budget_saturated"],
            color="#d62728",
            alpha=0.15,
            step="pre",
        )
        axes[1].plot(
            x,
            states["budget_reserved"] / 1048576,
            label="reserved",
            color="#d62728",
        )
        axes[1].plot(
            x,
            states["budget_available"] / 1048576,
            label="available",
            color="#2ca02c",
            alpha=0.8,
        )
    axes[0].set_title("Download Request Budget Utilization")
    axes[0].set_ylabel("reserved (%)")
    axes[0].set_ylim(0, 105)
    axes[1].set_ylabel("MiB")
    axes[1].set_xlabel("minutes since trace start")
    for axis in axes:
        axis.grid(True, alpha=0.25)
        if not states.empty:
            axis.legend(loc="best")
    fig.tight_layout()
    fig.savefig(path, dpi=150)
    plt.close(fig)


def write_blocking_plot(states: pd.DataFrame, path: Path) -> None:
    breakdown = pd.DataFrame(blocking_breakdown(states))
    fig, axis = plt.subplots(figsize=(10, 5))
    if not breakdown.empty:
        axis.bar(breakdown["blocking_class"], breakdown["percent"])
    axis.set_title("Approximate Blocking Breakdown")
    axis.set_ylabel("wall-clock time (%)")
    axis.set_ylim(0, 100)
    axis.tick_params(axis="x", labelrotation=30)
    axis.grid(True, axis="y", alpha=0.25)
    fig.tight_layout()
    fig.savefig(path, dpi=150)
    plt.close(fig)


def main() -> None:
    args = parse_args()
    db_path = build_cache(
        args.trace_dir,
        args.out,
        refresh_cache=args.refresh_cache,
    )
    analyze_database(db_path, args.out, args.bucket_seconds)
    print(f"wrote Zakura sync analysis to {args.out}")


if __name__ == "__main__":
    main()
