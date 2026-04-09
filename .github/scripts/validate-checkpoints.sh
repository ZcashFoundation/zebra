#!/usr/bin/env bash
# validate-checkpoints.sh
#
# Validates checkpoint file format and structure.
# Used by the checkpoint-update workflow to verify new entries before committing.
#
# Usage: .github/scripts/validate-checkpoints.sh <checkpoint-file>
#
# Exit codes:
#   0 — valid
#   1 — validation error (details printed to stderr)

set -euo pipefail

FILE="${1:?Usage: validate-checkpoints.sh <checkpoint-file>}"

if [ ! -f "$FILE" ]; then
  echo "ERROR: File not found: $FILE" >&2
  exit 1
fi

ERRORS=0

# Check every line matches the format: HEIGHT HASH (number, space, 64 hex chars)
BAD_FORMAT=$(grep -cvE '^[0-9]+ [0-9a-f]{64}$' "$FILE" || true)
if [ "$BAD_FORMAT" -gt 0 ]; then
  echo "ERROR: $BAD_FORMAT lines have invalid format (expected: HEIGHT HASH)" >&2
  grep -nvE '^[0-9]+ [0-9a-f]{64}$' "$FILE" | head -5 >&2
  ERRORS=$((ERRORS + BAD_FORMAT))
fi

# Check file starts at height 0
FIRST_HEIGHT=$(head -1 "$FILE" | cut -d' ' -f1)
if [ "$FIRST_HEIGHT" != "0" ]; then
  echo "ERROR: File must start at height 0, found height $FIRST_HEIGHT" >&2
  ERRORS=$((ERRORS + 1))
fi

# Check heights are monotonically increasing
NON_MONOTONIC=$(awk '{print $1}' "$FILE" | awk 'NR>1 && $1 <= prev {print NR": "$1" <= "prev} {prev=$1}')
if [ -n "$NON_MONOTONIC" ]; then
  COUNT=$(echo "$NON_MONOTONIC" | wc -l)
  echo "ERROR: $COUNT non-monotonic heights found" >&2
  echo "$NON_MONOTONIC" | head -5 >&2
  ERRORS=$((ERRORS + COUNT))
fi

# Check intervals (max 400 blocks between consecutive checkpoints)
LARGE_GAPS=$(awk '{print $1}' "$FILE" | awk 'NR>1 && ($1 - prev) > 400 {print NR": gap "($1-prev)" between "prev" and "$1} {prev=$1}')
if [ -n "$LARGE_GAPS" ]; then
  COUNT=$(echo "$LARGE_GAPS" | wc -l)
  echo "ERROR: $COUNT gaps exceed 400 blocks" >&2
  echo "$LARGE_GAPS" | head -5 >&2
  ERRORS=$((ERRORS + COUNT))
fi

# Check for duplicate heights
DUP_HEIGHTS=$(awk '{print $1}' "$FILE" | sort | uniq -d)
if [ -n "$DUP_HEIGHTS" ]; then
  COUNT=$(echo "$DUP_HEIGHTS" | wc -l)
  echo "ERROR: $COUNT duplicate heights" >&2
  echo "$DUP_HEIGHTS" | head -5 >&2
  ERRORS=$((ERRORS + COUNT))
fi

# Check for duplicate hashes
DUP_HASHES=$(awk '{print $2}' "$FILE" | sort | uniq -d)
if [ -n "$DUP_HASHES" ]; then
  COUNT=$(echo "$DUP_HASHES" | wc -l)
  echo "ERROR: $COUNT duplicate hashes" >&2
  echo "$DUP_HASHES" | head -5 >&2
  ERRORS=$((ERRORS + COUNT))
fi

TOTAL_LINES=$(wc -l < "$FILE" | tr -d ' ')
LAST_HEIGHT=$(tail -1 "$FILE" | cut -d' ' -f1)
echo "Validated $TOTAL_LINES entries, last height: $LAST_HEIGHT"

if [ "$ERRORS" -gt 0 ]; then
  echo "FAILED: $ERRORS validation errors" >&2
  exit 1
fi

echo "OK: All entries valid"
