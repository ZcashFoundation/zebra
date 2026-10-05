#!/usr/bin/env bash

set -euo pipefail

checker="$(git rev-parse --show-toplevel)/.github/scripts/check-release-safety.sh"
temporary_root="$(mktemp -d)"
fixture="${temporary_root}/repository"
stubs="${temporary_root}/stubs"
summary="${temporary_root}/summary.md"
output="${temporary_root}/output"

trap 'rm -rf "$temporary_root"' EXIT

mkdir -p "$fixture" "$stubs" "${temporary_root}/bin"

# Serves the fixture stored for the requested endpoint, or an empty result.
cat > "${temporary_root}/bin/gh" <<'STUB'
#!/usr/bin/env bash
endpoint="${*: -1}"
case "$endpoint" in
  */commits/*/check-runs\?*)
    key="checks-$(sed 's|.*/commits/\([^/]*\)/.*|\1|' <<< "$endpoint")"
    default='{"check_runs":[]}' ;;
  */issues\?*)
    key="issues-$(sed 's/.*labels=\([a-z]*\).*/\1/' <<< "$endpoint")"
    default='[]' ;;
  */issues/*/labels*)
    key=labels
    default='[]' ;;
  *)
    echo "gh stub: unexpected endpoint ${endpoint}" >&2
    exit 1 ;;
esac
file="${GH_STUB_DIR}/${key}"
if [[ ! -f "$file" ]]; then
  echo "$default"
elif [[ "$(cat "$file")" == "connection-failure" ]]; then
  echo "gh: connection reset by peer" >&2
  exit 1
else
  cat "$file"
fi
STUB
chmod +x "${temporary_root}/bin/gh"

export PATH="${temporary_root}/bin:${PATH}"
export GH_STUB_DIR="$stubs"
export GITHUB_REPOSITORY=ZcashFoundation/zebra
export GITHUB_STEP_SUMMARY="$summary"

g() {
  git -C "$fixture" "$@"
}

# commit <name> <date> <path>...: commits the paths and tags the commit with the name.
commit() {
  local name="$1" date="$2" path
  shift 2
  for path in "$@"; do
    mkdir -p "$(dirname "${fixture}/${path}")"
    echo "$name" >> "${fixture}/${path}"
  done
  g add --all
  GIT_AUTHOR_DATE="$date" GIT_COMMITTER_DATE="$date" g commit --quiet --message "$name"
  g tag "$name"
}

g init --quiet --initial-branch=main
g config user.email release-safety@example.com
g config user.name "Release Safety"
commit a 2026-09-01T12:00:00Z zebrad/src/main.rs
commit b 2026-09-02T12:00:00Z zebra-state/src/lib.rs
commit v1.0.1 2026-09-10T12:00:00Z zebrad/Cargo.toml
commit d 2026-09-12T12:00:00Z book/src/intro.md
commit v1.0.2 2026-09-20T12:00:00Z zebrad/Cargo.toml Cargo.lock
commit e 2026-09-22T12:00:00Z .github/workflows/ci.yml
commit f 2026-09-23T12:00:00Z book/src/release.md
g switch --quiet --create side-branch f
commit side 2026-09-24T12:00:00Z zebra-network/src/lib.rs
g switch --quiet main
g merge --quiet --no-ff --message merged side-branch
g tag merged

sha() {
  g rev-parse "$1"
}

# checks <revision> <conclusion>...: the sync-update check runs on a commit.
checks() {
  local revision="$1"
  shift
  jq -n -c '{check_runs: [$ARGS.positional[] | {conclusion: .}]}' --args "$@" \
    > "${stubs}/checks-$(sha "$revision")"
}

# issue <label> <number> <type> <created-at>: an open issue with the label.
issue() {
  jq -n -c --arg number "$2" --arg type "$3" --arg created "$4" \
    '[{number: ($number | tonumber), title: "Crash \($number)", type: {name: $type}, created_at: $created}]' \
    > "${stubs}/issues-$1"
}

override() {
  echo '[{"name":"release"},{"name":"release-override-safety"}]' > "${stubs}/labels"
}

begin() {
  rm -f "${stubs}"/*
  : > "$summary"
}

# check <status> <revision> <expected output>...
check() {
  local want="$1" revision="$2" status=0 pattern
  shift 2
  (cd "$fixture" && "$checker" "$(sha "$revision")" 42) > "$output" 2>&1 || status=$?
  if [[ "$status" -ne "$want" ]]; then
    echo "expected status ${want}, got ${status}: ${revision}" >&2
    cat "$output" >&2
    exit 1
  fi
  for pattern; do
    if ! grep -Fq -- "$pattern" "$output"; then
      echo "expected output to contain: ${pattern}" >&2
      cat "$output" >&2
      exit 1
    fi
  done
}

# A passing run on the base commit, or on an older commit after the last node change, passes.
begin
checks f success
check 0 f

begin
checks v1.0.2 success
check 0 f

# Cancelled and failed runs do not count.
begin
checks f cancelled failure
check 1 f "::error title=Release safety::" "$(sha v1.0.2)" "gh workflow run zfnd-ci-integration-tests-gcp.yml --ref main"
grep -Fq "gh workflow run" "$summary"

# A run before the last node change does not cover it.
begin
checks d success
check 1 f "$(sha v1.0.2)"

# A passing run on a merged side branch does not count; the merge changed node files.
begin
checks side success
check 1 merged "$(sha merged)"

# Bugs opened after the previous release block it; older bugs and other issue types do not.
begin
checks f success
issue urgent 8 Bug 2026-09-21T00:00:00Z
check 1 f "#8 Crash 8" "after v1.0.2"

begin
checks f success
issue security 9 Bug 2026-09-15T00:00:00Z
issue urgent 8 Task 2026-09-21T00:00:00Z
check 0 f

# The override label turns both failures into warnings.
begin
issue security 7 Bug 2026-09-21T00:00:00Z
override
check 0 f "::warning title=Release safety::No passing" "::warning title=Release safety::Open Bug issues" "#7 Crash 7"
grep -Fq "::error" "$output" && exit 1

# A failed gh call fails the check.
begin
echo connection-failure > "${stubs}/issues-security"
checks f success
check 1 f "connection reset by peer"

# The previous release tag is required.
begin
check 128 b "No tags can describe"
