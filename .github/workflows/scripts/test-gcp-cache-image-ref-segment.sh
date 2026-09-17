#!/usr/bin/env bash

set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
segment_script="${repository_root}/.github/workflows/scripts/gcp-cache-image-ref-segment.sh"

# The longest components the callers compose around this segment add up to 51
# characters, out of the 63 GCE allows for an image name.
SEGMENT_BUDGET=12

run_segment() {
  local repository="$1"
  local ref_slug="$2"

  GITHUB_REPOSITORY="$repository" \
    GITHUB_REPOSITORY_ID=1234567890 \
    REF_SLUG="$ref_slug" \
    "$segment_script"
}

assert_segment() {
  local repository="$1"
  local ref_slug="$2"
  local expected="$3"
  local actual

  actual="$(run_segment "$repository" "$ref_slug")"
  if [[ "$actual" != "$expected" ]]; then
    echo "expected ${repository}@${ref_slug} to produce '${expected}', got '${actual}'" >&2
    exit 1
  fi
  if [[ "${#actual}" -gt "$SEGMENT_BUDGET" ]]; then
    echo "segment '${actual}' is ${#actual} characters, over the budget of ${SEGMENT_BUDGET}" >&2
    exit 1
  fi
}

assert_segment ZcashFoundation/zebra main main
assert_segment ZcashFoundation/zebra 11415-merge 11415-merge
assert_segment ZcashFoundation/zebra ci-canonical-repository-guards ci-canonical
assert_segment ZcashFoundation/zebra-private main r1234567890
assert_segment contributor/zebra main r1234567890

# A copy on a canonical branch name must not produce the canonical segment.
canonical="$(run_segment ZcashFoundation/zebra main)"
copy="$(run_segment contributor/zebra main)"
if [[ "$canonical" == "$copy" ]]; then
  echo "a repository copy produced the canonical segment '${copy}'" >&2
  exit 1
fi

echo "gcp-cache-image-ref-segment.sh tests passed"
