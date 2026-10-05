#!/usr/bin/env bash

set -euo pipefail

base="$(git rev-parse --verify "${1:?usage: $0 <base-revision> <release-pr-number>}^{commit}")"
pr="${2:?usage: $0 <base-revision> <release-pr-number>}"
repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must be set}"

check_query="Zebra%20tip%20update%20%2F%20Run%20sync-update-mainnet%20test"
node_paths=('*.rs' '*-checkpoints.txt' '*Cargo.toml' '*Cargo.lock' 'rust-toolchain.toml' '.cargo/**' 'docker/**')

failures=()

last_change="$(git rev-list -1 --first-parent "$base" -- "${node_paths[@]}")"
passed=""
for sha in $(git rev-list --first-parent "${last_change}^1..${base}"); do
  passed="$(gh api "repos/${repo}/commits/${sha}/check-runs?check_name=${check_query}&filter=all" \
    | jq -r '[.check_runs[] | select(.conclusion == "success")] | length')"
  [[ "$passed" -eq 0 ]] || break
done
if [[ "$passed" -eq 0 ]]; then
  failures+=("No passing 'Zebra tip update / Run sync-update-mainnet test' on ${last_change}, the last commit that changed node files, or any later first-parent commit. Start a run with: gh workflow run zfnd-ci-integration-tests-gcp.yml --ref main")
fi

previous="$(git describe --tags --abbrev=0 --match 'v[0-9]*' "$base")"
since="$(TZ=UTC git log -1 --format=%cd --date=format-local:%Y-%m-%dT%H:%M:%SZ "${previous}^{commit}")"
bugs="$({
  gh api --paginate "repos/${repo}/issues?state=open&labels=security&per_page=100" &&
  gh api --paginate "repos/${repo}/issues?state=open&labels=urgent&per_page=100"
} | jq -r -s --arg since "$since" '
  [.[][] | select(.pull_request == null and .type.name == "Bug" and .created_at > $since)]
  | unique_by(.number)
  | map("#\(.number) \(.title | gsub("[\r\n]"; " "))")
  | join("; ")')"
if [[ -n "$bugs" ]]; then
  failures+=("Open Bug issues labelled security or urgent were opened after ${previous} (${since}): ${bugs}")
fi

[[ ${#failures[@]} -gt 0 ]] || exit 0

level=error
if gh api "repos/${repo}/issues/${pr}/labels?per_page=100" | jq -e 'any(.[]; .name == "release-override-safety")' > /dev/null; then
  level=warning
fi
for failure in "${failures[@]}"; do
  echo "::${level} title=Release safety::${failure}"
  echo "${failure}"$'\n' >> "${GITHUB_STEP_SUMMARY:-/dev/null}"
done
[[ "$level" == warning ]]
