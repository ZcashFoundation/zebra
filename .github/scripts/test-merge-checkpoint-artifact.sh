#!/usr/bin/env bash
# Tests merge-checkpoint-artifact.sh with valid and malformed checkpoint artifacts.

set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
merge="${repository_root}/.github/scripts/merge-checkpoint-artifact.sh"
temporary_root="$(mktemp -d)"
trap 'rm -rf "$temporary_root"' EXIT

failures=0
hash_of() { printf '%064x' "$1"; }

repo_file="${temporary_root}/main-checkpoints.txt"
for height in 0 400 800; do
	echo "${height} $(hash_of "$((height + 1))")"
done >"$repo_file"

new_artifact() {
	local name="$1"
	mkdir -p "${temporary_root}/${name}"
	echo "${temporary_root}/${name}"
}

expect() {
	local expected="$1" description="$2" artifact_dir="$3"
	local out="${temporary_root}/out-${RANDOM}.txt"
	if "$merge" "$artifact_dir" main-checkpoints.txt "$repo_file" "$out" >/dev/null 2>&1; then
		actual=pass
	else
		actual=fail
	fi
	if [ "$actual" = "$expected" ]; then
		echo "ok: ${description}"
	else
		echo "FAILED: ${description}: expected ${expected}, got ${actual}" >&2
		failures=$((failures + 1))
	fi
	if [ "$actual" = fail ] && [ -e "$out" ]; then
		echo "FAILED: ${description}: wrote output after rejecting the artifact" >&2
		failures=$((failures + 1))
	fi
}

valid_lines() {
	cat "$repo_file"
	echo "1200 $(hash_of 1201)"
	echo "1600 $(hash_of 1601)"
}

dir=$(new_artifact valid)
valid_lines >"${dir}/main-checkpoints.txt"
expect pass "valid artifact with new entries" "$dir"

out="${temporary_root}/merged.txt"
"$merge" "$dir" main-checkpoints.txt "$repo_file" "$out" >/dev/null
if [ "$(head -n 3 "$out")" != "$(cat "$repo_file")" ] || [ "$(wc -l <"$out")" -ne 5 ]; then
	echo "FAILED: merged file must be the repository file plus the new entries" >&2
	failures=$((failures + 1))
fi

dir=$(new_artifact merged-again)
cp "$out" "${dir}/main-checkpoints.txt"
"$merge" "$dir" main-checkpoints.txt "$repo_file" "${temporary_root}/merged-again.txt" >/dev/null
if ! cmp -s "$out" "${temporary_root}/merged-again.txt"; then
	echo "FAILED: merging an already merged file must be idempotent" >&2
	failures=$((failures + 1))
fi

dir=$(new_artifact extra-file)
valid_lines >"${dir}/main-checkpoints.txt"
mkdir -p "${dir}/.github/scripts"
echo 'exit 0' >"${dir}/.github/scripts/validate-checkpoints.sh"
expect fail "artifact with an extra file" "$dir"

dir=$(new_artifact wrong-name)
valid_lines >"${dir}/test-checkpoints.txt"
expect fail "artifact with the wrong file name" "$dir"

dir=$(new_artifact symlink)
ln -s "$repo_file" "${dir}/main-checkpoints.txt"
expect fail "artifact file is a symlink" "$dir"

dir=$(new_artifact empty)
: >"${dir}/main-checkpoints.txt"
expect fail "empty artifact file" "$dir"

dir=$(new_artifact shell-text)
{
	valid_lines
	# shellcheck disable=SC2016 # literal shell text, must not expand
	echo '$(id) x'
} >"${dir}/main-checkpoints.txt"
expect fail "line that is not a checkpoint" "$dir"

dir=$(new_artifact crlf)
valid_lines | sed 's/$/\r/' >"${dir}/main-checkpoints.txt"
expect fail "CRLF line endings" "$dir"

dir=$(new_artifact uppercase)
{
	cat "$repo_file"
	echo "1200 $(hash_of 1201 | tr 'a-f' 'A-F')"
} >"${dir}/main-checkpoints.txt"
expect fail "uppercase hash" "$dir"

dir=$(new_artifact unordered)
{
	cat "$repo_file"
	echo "1600 $(hash_of 1601)"
	echo "1200 $(hash_of 1201)"
} >"${dir}/main-checkpoints.txt"
expect fail "heights not increasing" "$dir"

dir=$(new_artifact gap)
{
	cat "$repo_file"
	echo "1300 $(hash_of 1301)"
} >"${dir}/main-checkpoints.txt"
expect fail "gap of more than 400 blocks" "$dir"

dir=$(new_artifact duplicate-hash)
{
	cat "$repo_file"
	echo "1200 $(hash_of 801)"
} >"${dir}/main-checkpoints.txt"
expect fail "hash already in the repository file" "$dir"

dir=$(new_artifact huge-height)
{
	cat "$repo_file"
	echo "12345678901 $(hash_of 1201)"
} >"${dir}/main-checkpoints.txt"
expect fail "height with more than 10 digits" "$dir"

if [ "$failures" -gt 0 ]; then
	echo "${failures} checkpoint artifact test(s) failed" >&2
	exit 1
fi
echo "All checkpoint artifact tests passed"
