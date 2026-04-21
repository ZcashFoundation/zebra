#!/usr/bin/env bash
# validate-checkpoints.sh
#
# Validates checkpoint file format and structure in a single pass.
#
# Usage: .github/scripts/validate-checkpoints.sh <checkpoint-file>
#
# Exit codes:
#   0 - valid
#   1 - validation error (details printed to stderr)

set -euo pipefail

FILE="${1:?Usage: validate-checkpoints.sh <checkpoint-file>}"

if [ ! -f "$FILE" ]; then
  echo "ERROR: File not found: $FILE" >&2
  exit 1
fi

# Single-pass validation using awk
awk '
  !/^[0-9]+ [0-9a-f]{64}$/ {
    printf "ERROR: line %d: invalid format: %s\n", NR, $0 > "/dev/stderr"
    errs++
    next
  }
  NR == 1 && $1 != "0" {
    printf "ERROR: file must start at height 0, found %s\n", $1 > "/dev/stderr"
    errs++
  }
  NR > 1 && $1 <= prev_h {
    printf "ERROR: line %d: height %d is not greater than previous %d\n", NR, $1, prev_h > "/dev/stderr"
    errs++
  }
  NR > 1 && ($1 - prev_h) > 400 {
    printf "ERROR: line %d: gap of %d blocks between %d and %d\n", NR, $1 - prev_h, prev_h, $1 > "/dev/stderr"
    errs++
  }
  $1 in seen_h {
    printf "ERROR: line %d: duplicate height %d\n", NR, $1 > "/dev/stderr"
    errs++
  }
  $2 in seen_hash {
    printf "ERROR: line %d: duplicate hash %s\n", NR, $2 > "/dev/stderr"
    errs++
  }
  {
    seen_h[$1] = 1
    seen_hash[$2] = 1
    prev_h = $1
    last_h = $1
    total++
  }
  END {
    printf "Validated %d entries, last height: %d\n", total, last_h
    if (errs > 0) {
      printf "FAILED: %d validation errors\n", errs > "/dev/stderr"
      exit 1
    }
    print "OK: All entries valid"
  }
' "$FILE"
