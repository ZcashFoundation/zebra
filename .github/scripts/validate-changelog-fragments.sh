#!/usr/bin/env bash

# Dry-runs the release changelog batching for every changie project, so a
# pending change fragment that would break the Release PR job fails here first.
#
# Every fragment under `.changes/unreleased/` is checked, not only the ones a pull
# request touches, because fragments from separate pull requests are combined on
# `main`.

set -euo pipefail

if ! command -v changie >/dev/null 2>&1; then
  echo "changie is required to validate change fragments; see https://changie.dev/guide/installation/" >&2
  exit 1
fi

cd "$(git rev-parse --show-toplevel)"

projects="$(awk '/^projects:/ { p = 1; next } /^[^[:space:]#]/ { p = 0 } p && $1 == "key:" { print $2 }' .changie.yaml)"
failed=false
reported=""

while IFS= read -r project; do
  if output="$(changie batch major --dry-run --allow-no-changes --project "$project" 2>&1 >/dev/null </dev/null)"; then
    continue
  fi

  failed=true
  message="${output#Error: }"
  message="${message//$'\n'/%0A}"
  printf 'changie cannot batch %s: %s\n' "$project" "$message" >&2

  # Changie reads every fragment for each project, so a fragment it cannot parse
  # fails every project with the same message. Annotate it once.
  if ! grep -qxF -- "$message" <<< "$reported"; then
    reported+="${message}"$'\n'
    echo "::error title=Invalid change fragment::${message}" >&2
  fi
done <<< "$projects"

[[ "$failed" == "false" ]]
