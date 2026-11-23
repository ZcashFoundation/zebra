#!/usr/bin/env bash
# merge-checkpoint-artifact.sh
#
# Reads a downloaded checkpoint artifact as data, and appends the
# checkpoints it has above the repository's last checkpoint.
#
# Usage: .github/scripts/merge-checkpoint-artifact.sh <artifact-dir> <file-name> <repo-file> <out-file>
#
# <artifact-dir> must be a scratch directory outside the checkout, holding
# exactly one regular file named <file-name>. The file is only read as text:
# nothing in it is executed. <out-file> is <repo-file> followed by the new
# entries, and is checked with validate-checkpoints.sh.
#
# Exit codes:
#   0 - <out-file> written and valid
#   1 - the artifact or the merged file failed a check (details on stderr)

set -euo pipefail

ARTIFACT_DIR="${1:?Usage: merge-checkpoint-artifact.sh <artifact-dir> <file-name> <repo-file> <out-file>}"
FILE_NAME="${2:?missing <file-name>}"
REPO_FILE="${3:?missing <repo-file>}"
OUT_FILE="${4:?missing <out-file>}"

# Generated files are about 1 MB; anything far larger is not a checkpoint list.
MAX_BYTES=$((16 * 1024 * 1024))

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

[ -d "$ARTIFACT_DIR" ] && [ ! -L "$ARTIFACT_DIR" ] || fail "artifact directory not found: ${ARTIFACT_DIR}"
[ -f "$REPO_FILE" ] || fail "repository checkpoint file not found: ${REPO_FILE}"

# The artifact must contain exactly the expected file and nothing else.
ENTRIES=$(find "$ARTIFACT_DIR" -mindepth 1 -printf '%P\n' | LC_ALL=C sort)
[ "$ENTRIES" = "$FILE_NAME" ] || fail "artifact must contain only ${FILE_NAME}, found: $(printf '%s' "$ENTRIES" | tr '\n' ' ')"

ARTIFACT_FILE="${ARTIFACT_DIR}/${FILE_NAME}"
[ -f "$ARTIFACT_FILE" ] && [ ! -L "$ARTIFACT_FILE" ] || fail "${FILE_NAME} is not a regular file"

SIZE=$(stat -c %s "$ARTIFACT_FILE")
[ "$SIZE" -gt 0 ] || fail "${FILE_NAME} is empty"
[ "$SIZE" -le "$MAX_BYTES" ] || fail "${FILE_NAME} is ${SIZE} bytes, limit is ${MAX_BYTES}"

# Every line must be "<height> <64 lowercase hex chars>", with no other bytes.
# Heights are capped at 10 digits so awk compares them exactly. Ordering, gaps
# and duplicates are checked by validate-checkpoints.sh on the merged file,
# which holds every artifact line that is kept.
if BAD=$(LC_ALL=C grep -n -v -m 5 -E '^[0-9]{1,10} [0-9a-f]{64}$' "$ARTIFACT_FILE"); then
  fail "${FILE_NAME} has invalid lines (first 5): ${BAD}"
fi

CURRENT_LAST=$(tail -n 1 "$REPO_FILE" | awk '{ print $1 }')
[[ "$CURRENT_LAST" =~ ^[0-9]{1,10}$ ]] || fail "cannot read the last height of ${REPO_FILE}"

TMP_OUT=$(mktemp)
trap 'rm -f "$TMP_OUT"' EXIT

cp "$REPO_FILE" "$TMP_OUT"
LC_ALL=C awk -v last="$CURRENT_LAST" '($1 + 0) > (last + 0)' "$ARTIFACT_FILE" >> "$TMP_OUT"

NEW_COUNT=$(( $(wc -l < "$TMP_OUT") - $(wc -l < "$REPO_FILE") ))
echo "${FILE_NAME}: $(wc -l < "$ARTIFACT_FILE" | tr -d ' ') lines in artifact, last repository height ${CURRENT_LAST}, ${NEW_COUNT} new"

"${SCRIPT_DIR}/validate-checkpoints.sh" "$TMP_OUT"

mv "$TMP_OUT" "$OUT_FILE"
trap - EXIT
