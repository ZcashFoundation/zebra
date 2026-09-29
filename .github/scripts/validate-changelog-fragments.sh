#!/usr/bin/env bash

# Reads every pending change fragment the way changie does when it batches them
# into a release, and reports each fragment changie rejects by path.
#
# Run from a checkout that has the fragments to check. Every fragment under
# `.changes/unreleased/` is checked, not only the ones a pull request touches,
# because fragments from separate pull requests are combined on `main`.

set -euo pipefail

if ! command -v changie >/dev/null 2>&1; then
  echo "changie is required to validate change fragments; see https://changie.dev/guide/installation/" >&2
  exit 1
fi

repository_root="$(git rev-parse --show-toplevel)"

cd "$repository_root"

unreleased_directory=".changes/unreleased"
scratch="$(mktemp -d)"

trap 'rm -rf "$scratch"' EXIT

# Changie loads only the requested project's fragments, and rejects an unknown
# kind only in those. Without a project list it loads every fragment.
awk '/^projects:/ { skip = 1; next } /^[^[:space:]#]/ { skip = 0 } !skip' .changie.yaml > "${scratch}/.changie.yaml"
mkdir -p "${scratch}/${unreleased_directory}"

failed=false

while IFS= read -r -d '' fragment; do
  cp "$fragment" "${scratch}/${fragment}"

  if ! output="$(cd "$scratch" && changie batch major --dry-run 2>&1 >/dev/null)"; then
    message="${output#Error: }"
    echo "::error file=${fragment},title=Invalid change fragment::${message//$'\n'/%0A}" >&2
    failed=true
  fi

  rm "${scratch}/${fragment}"
done < <(find "$unreleased_directory" -maxdepth 1 -name '*.yaml' -print0 | sort -z)

if [[ "$failed" == "true" ]]; then
  exit 1
fi
