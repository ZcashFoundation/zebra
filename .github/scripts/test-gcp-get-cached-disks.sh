#!/usr/bin/env bash
# A longer cache scope must not shadow the current scope through an unanchored prefix.
set -euo pipefail

GITHUB_WORKSPACE="$(git rev-parse --show-toplevel)"
GITHUB_REPOSITORY=ZcashFoundation/zebra
CACHE_SCOPE=ci
DISK_PREFIX=lwd-cache
DISK_SUFFIX=tip
NETWORK=mainnet

# Return candidates newest first, applying the same regex filter as the image API.
gcloud() {
    if [[ "$1 $2 $3" == "compute images describe" ]]; then
        return 0
    fi

    local argument pattern="" image
    for argument in "$@"; do
        if [[ "${argument}" == --filter=* ]]; then
            pattern="${argument#*name~}"
        fi
    done
    for image in \
        "lwd-cache-lwd-cache-ci-deadbee-v${LOCAL_STATE_VERSION}-mainnet-tip" \
        "lwd-cache-ci-beefbee-v${LOCAL_STATE_VERSION}-mainnet-tip" \
        "lwd-cache-main-faded00-v${LOCAL_STATE_VERSION}-mainnet-tip"; do
        if [[ "${image}" =~ ${pattern} ]]; then
            printf '%s\n' "${image}"
        fi
    done
}

# shellcheck source=.github/workflows/scripts/gcp-get-cached-disks.sh
source "${GITHUB_WORKSPACE}/.github/workflows/scripts/gcp-get-cached-disks.sh" >/dev/null

expected="lwd-cache-ci-beefbee-v${LOCAL_STATE_VERSION}-mainnet-tip"
if [[ "${CACHED_DISK_NAME}" != "${expected}" ]]; then
    printf 'Expected %s, got %s\n' "${expected}" "${CACHED_DISK_NAME}" >&2
    exit 1
fi
printf 'ok: the current cache scope wins over a newer suffix match\n'
