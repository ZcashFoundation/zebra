#!/usr/bin/env bash

# Prints the ref segment of a shared GCP cache-image name: the branch slug in
# ZcashFoundation/zebra, the numeric repository id elsewhere.
# Reads REF_SLUG, GITHUB_REPOSITORY and GITHUB_REPOSITORY_ID.
# Callers reserve 12 characters for this segment (GCE image names cap at 63).

set -euo pipefail

if [[ "${GITHUB_REPOSITORY}" == "ZcashFoundation/zebra" ]]; then
    printf '%s' "${REF_SLUG:0:12}"
else
    printf 'r%s' "${GITHUB_REPOSITORY_ID}"
fi
