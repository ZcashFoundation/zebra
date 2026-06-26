#!/usr/bin/env bash

# Slice one release's section out of the root CHANGELOG.md.
#
# Given a Zebra version, prints every line from that version's
# "## [Zebra <version>]" heading up to (but not including) the next
# "## [" heading. This is the deterministic stage of the release-notes
# builder: the AI stage only wraps this text with operational context and
# must not add, reword, or reorder the curated entries.
#
# Usage:
#   extract-changelog-section.sh <version> [changelog-file] [output-file]
#
# Environment overrides (used when CI passes values via env):
#   VERSION, CHANGELOG_FILE, SECTION_FILE
#
# Step outputs (when GITHUB_OUTPUT is set):
#   version, tag, previous_tag, section_path

set -euo pipefail

VERSION="${1:-${VERSION:-}}"
CHANGELOG_FILE="${2:-${CHANGELOG_FILE:-CHANGELOG.md}}"
SECTION_FILE="${3:-${SECTION_FILE:-}}"

if [[ -z "${VERSION}" ]]; then
    echo "Usage: extract-changelog-section.sh <version> [changelog-file] [output-file]" >&2
    exit 1
fi

if [[ ! -f "${CHANGELOG_FILE}" ]]; then
    echo "Changelog file not found: ${CHANGELOG_FILE}" >&2
    exit 1
fi

if [[ -z "${SECTION_FILE}" ]]; then
    SECTION_FILE="$(mktemp)"
fi

awk -v version="${VERSION}" '
    BEGIN { heading = "## [Zebra " version "]" }
    /^##[[:space:]]+\[/ {
        if (found) { exit }
        if (index($0, heading) == 1) { found = 1 }
    }
    found { print }
' "${CHANGELOG_FILE}" > "${SECTION_FILE}"

if [[ ! -s "${SECTION_FILE}" ]]; then
    echo "No changelog section found for Zebra ${VERSION} in ${CHANGELOG_FILE}" >&2
    exit 1
fi

# The first "## [Zebra X.Y.Z]" heading below the current section is the
# previous release; use it for the compare link.
PREVIOUS_VERSION="$(awk -v version="${VERSION}" '
    /^## \[Zebra [0-9]+\.[0-9]+\.[0-9]+\]/ {
        match($0, /[0-9]+\.[0-9]+\.[0-9]+/)
        found_version = substr($0, RSTART, RLENGTH)
        if (seen) { print found_version; exit }
        if (found_version == version) { seen = 1 }
    }
' "${CHANGELOG_FILE}")"

echo "Extracted Zebra ${VERSION} changelog section to ${SECTION_FILE}" >&2
if [[ -n "${PREVIOUS_VERSION}" ]]; then
    echo "Previous release: ${PREVIOUS_VERSION}" >&2
fi

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    {
        echo "version=${VERSION}"
        echo "tag=v${VERSION}"
        echo "section_path=${SECTION_FILE}"
        [[ -n "${PREVIOUS_VERSION}" ]] && echo "previous_tag=v${PREVIOUS_VERSION}"
    } >> "${GITHUB_OUTPUT}"
else
    cat "${SECTION_FILE}"
fi
