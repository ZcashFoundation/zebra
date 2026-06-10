#!/usr/bin/env bash
# Resolve hash-pinned zcashd compat artifacts from zebrad/zcashd-compat-manifest.json.
set -euo pipefail

DEFAULT_MANIFEST="zebrad/zcashd-compat-manifest.json"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MANIFEST_PATH=""
TARGET_TRIPLE=""
DOCKER_PLATFORM=""
CONTEXT_DIR=""
WRITE_GITHUB_OUTPUT=0
REQUIRE_TARGETS=""

usage() {
  cat <<'EOF'
Usage: resolve-zcashd-compat-manifest.sh [options]

Reads the committed zcashd compat manifest and either exports GitHub Actions
outputs or prepares a Docker build context containing bin/zcashd.

Options:
  --manifest-path PATH       Manifest JSON path (default: zebrad/zcashd-compat-manifest.json)
  --target-triple TRIPLE     Rust-style target triple
  --platform PLATFORM        Docker platform (linux/amd64 or linux/arm64)
  --require-targets LIST     Comma-separated target triples that must be present
  --write-github-output      Write release_tag/url_amd64/sha256_amd64/url_arm64/sha256_arm64
  --prepare-build-context DIR
                             Download, verify, and install zcashd into DIR/bin/zcashd
  -h, --help                 Show this help
EOF
}

abs_manifest_path() {
  if [[ -n "$MANIFEST_PATH" ]]; then
    if [[ "$MANIFEST_PATH" = /* ]]; then
      echo "$MANIFEST_PATH"
    else
      echo "$REPO_ROOT/$MANIFEST_PATH"
    fi
  else
    echo "$REPO_ROOT/$DEFAULT_MANIFEST"
  fi
}

platform_to_target_triple() {
  case "$1" in
    linux/amd64) echo "x86_64-pc-linux-gnu" ;;
    linux/arm64) echo "aarch64-linux-gnu" ;;
    *)
      echo "unsupported Docker platform for zcashd compat artifacts: $1" >&2
      exit 1
      ;;
  esac
}

target_to_github_prefix() {
  case "$1" in
    x86_64-pc-linux-gnu) echo "amd64" ;;
    aarch64-linux-gnu) echo "arm64" ;;
    *)
      echo "unsupported target triple for GitHub Actions outputs: $1" >&2
      exit 1
      ;;
  esac
}

validate_manifest() {
  local manifest="$1"

  if [[ ! -f "$manifest" ]]; then
    echo "zcashd compat manifest not found: $manifest" >&2
    exit 1
  fi

  jq -e '
    .schema_version
    and (.release_tag | type == "string" and length > 0)
    and (.artifacts | type == "array" and length > 0)
  ' "$manifest" >/dev/null

  local target
  while IFS= read -r target; do
    jq -e --arg target "$target" '
      .artifacts[]
      | select(.target_triple == $target)
      | .runtime_archive_url
      and .runtime_archive_sha256
      and .runtime_archive_member_binary_path
    ' "$manifest" >/dev/null
  done < <(jq -r '.artifacts[].target_triple' "$manifest")

  local unique_targets
  unique_targets="$(jq -r '.artifacts[].target_triple' "$manifest" | sort -u | wc -l | tr -d ' ')"
  local total_targets
  total_targets="$(jq -r '.artifacts | length' "$manifest")"
  if [[ "$unique_targets" != "$total_targets" ]]; then
    echo "duplicate target triple found in zcashd compat manifest: $manifest" >&2
    exit 1
  fi
}

artifact_field() {
  local manifest="$1"
  local target_triple="$2"
  local field="$3"

  jq -er --arg target "$target_triple" --arg field "$field" '
    .artifacts[]
    | select(.target_triple == $target)
    | .[$field]
  ' "$manifest"
}

require_targets_present() {
  local manifest="$1"
  local missing=0
  local triple

  IFS=',' read -ra required <<< "$REQUIRE_TARGETS"
  for triple in "${required[@]}"; do
    triple="${triple// /}"
    if [[ -z "$triple" ]]; then
      continue
    fi

    if ! jq -e --arg target "$triple" '
      [.artifacts[] | select(.target_triple == $target)] | length == 1
    ' "$manifest" >/dev/null; then
      echo "missing required zcashd compat artifact for target triple: $triple" >&2
      missing=1
    fi
  done

  if [[ "$missing" -ne 0 ]]; then
    exit 1
  fi
}

write_github_output() {
  local manifest="$1"
  local output_file="${GITHUB_OUTPUT:-}"

  if [[ -z "$output_file" ]]; then
    echo "GITHUB_OUTPUT is required for --write-github-output" >&2
    exit 1
  fi

  {
    echo "release_tag=$(jq -r '.release_tag' "$manifest")"
    echo "manifest_path=$manifest"
  } >> "$output_file"

  local triple prefix url sha256
  for triple in x86_64-pc-linux-gnu aarch64-linux-gnu; do
    if jq -e --arg target "$triple" '
      [.artifacts[] | select(.target_triple == $target)] | length == 1
    ' "$manifest" >/dev/null; then
      prefix="$(target_to_github_prefix "$triple")"
      url="$(artifact_field "$manifest" "$triple" "runtime_archive_url")"
      sha256="$(artifact_field "$manifest" "$triple" "runtime_archive_sha256")"
      {
        echo "url_${prefix}=$url"
        echo "sha256_${prefix}=$sha256"
      } >> "$output_file"
    fi
  done
}

prepare_build_context() {
  local manifest="$1"
  local target_triple="$2"
  local context_dir="$3"
  local url sha256 member_path archive_path extract_dir work_dir binary_source binary_dest

  url="$(artifact_field "$manifest" "$target_triple" "runtime_archive_url")"
  sha256="$(artifact_field "$manifest" "$target_triple" "runtime_archive_sha256")"
  member_path="$(artifact_field "$manifest" "$target_triple" "runtime_archive_member_binary_path")"

  work_dir="$(mktemp -d)"
  trap 'rm -rf "$work_dir"' RETURN

  archive_path="$work_dir/archive.tar.gz"
  extract_dir="$work_dir/extracted"

  curl -fsSL "$url" -o "$archive_path"
  echo "$sha256  $archive_path" | sha256sum -c -
  mkdir -p "$extract_dir"
  tar -xzf "$archive_path" -C "$extract_dir"

  binary_source="$extract_dir/${member_path#./}"
  if [[ ! -x "$binary_source" ]]; then
    echo "expected executable missing from zcashd compat archive: $member_path" >&2
    exit 1
  fi

  rm -rf "$context_dir"
  binary_dest="$context_dir/bin/zcashd"
  mkdir -p "$(dirname "$binary_dest")"
  install -D -m 0755 "$binary_source" "$binary_dest"
  "$binary_dest" --version
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --manifest-path)
      MANIFEST_PATH="$2"
      shift 2
      ;;
    --target-triple)
      TARGET_TRIPLE="$2"
      shift 2
      ;;
    --platform)
      DOCKER_PLATFORM="$2"
      shift 2
      ;;
    --require-targets)
      REQUIRE_TARGETS="$2"
      shift 2
      ;;
    --write-github-output)
      WRITE_GITHUB_OUTPUT=1
      shift
      ;;
    --prepare-build-context)
      CONTEXT_DIR="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

manifest="$(abs_manifest_path)"
validate_manifest "$manifest"

if [[ -n "$REQUIRE_TARGETS" ]]; then
  require_targets_present "$manifest"
fi

if [[ -n "$DOCKER_PLATFORM" ]]; then
  if [[ -n "$TARGET_TRIPLE" && "$TARGET_TRIPLE" != "$(platform_to_target_triple "$DOCKER_PLATFORM")" ]]; then
    echo "conflicting --platform and --target-triple arguments" >&2
    exit 1
  fi
  TARGET_TRIPLE="$(platform_to_target_triple "$DOCKER_PLATFORM")"
fi

if [[ "$WRITE_GITHUB_OUTPUT" -eq 1 ]]; then
  write_github_output "$manifest"
fi

if [[ -n "$CONTEXT_DIR" ]]; then
  if [[ -z "$TARGET_TRIPLE" ]]; then
    echo "--prepare-build-context requires --target-triple or --platform" >&2
    exit 1
  fi
  prepare_build_context "$manifest" "$TARGET_TRIPLE" "$CONTEXT_DIR"
fi

if [[ "$WRITE_GITHUB_OUTPUT" -eq 0 && -z "$CONTEXT_DIR" ]]; then
  echo "no action requested; pass --write-github-output and/or --prepare-build-context" >&2
  exit 1
fi
