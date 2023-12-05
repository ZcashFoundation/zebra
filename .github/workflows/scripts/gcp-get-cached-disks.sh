#!/usr/bin/env bash

# Description:
# This script finds a cached Google Cloud Compute image based on specific criteria.
# It prioritizes images from the current commit, falls back to the main branch,
# and finally checks other branches if needed. The selected image is used for
# setting up the environment in a CI/CD pipeline.

set -euxo pipefail

# Function to find and report a cached disk image
find_cached_disk_image() {
    local search_pattern="${1}"
    local description="${2}"
    local disk_name

    echo "Searching for ${description} disk..."
    disk_name=$(gcloud compute images list --filter="status=READY AND name~${search_pattern}" --format="value(NAME)" --sort-by=~creationTimestamp --limit=1)

    if [[ -n "${disk_name}" ]]; then
        echo "Found ${description} Disk: ${disk_name}"
        echo "Description: $(gcloud compute images describe ${disk_name} --format='value(DESCRIPTION)')"
    else
        echo "No ${description} disk found."
    fi

    echo "${disk_name}"
}

# Extract local state version
echo "Extracting local state version..."
LOCAL_STATE_VERSION=$(grep -oE "DATABASE_FORMAT_VERSION: .* [0-9]+" "${GITHUB_WORKSPACE}/zebra-state/src/constants.rs" | grep -oE "[0-9]+" | tail -n1)
echo "STATE_VERSION: ${LOCAL_STATE_VERSION}"

# Define DISK_PREFIX based on the requiring state directory
if [[ "${NEEDS_LWD_STATE}" == "true" ]]; then
    DISK_PREFIX="${LWD_STATE_DIR}"
else
    DISK_PREFIX="${ZEBRA_STATE_DIR:-${DISK_PREFIX}}"
fi

# Find the most suitable cached disk image
echo "Finding the most suitable cached disk image..."
COMMIT_DISK_PREFIX="${DISK_PREFIX}-.+-${GITHUB_SHA_SHORT}-v${LOCAL_STATE_VERSION}-${NETWORK}-${DISK_SUFFIX}"
CACHED_DISK_NAME=$(find_cached_disk_image "${COMMIT_DISK_PREFIX}" "commit")

if [[ -z "${CACHED_DISK_NAME}" && "${PREFER_MAIN_CACHED_STATE}" == "true" ]]; then
    MAIN_DISK_PREFIX="${DISK_PREFIX}-main-[0-9a-f]+-v${LOCAL_STATE_VERSION}-${NETWORK}-${DISK_SUFFIX}"
    CACHED_DISK_NAME=$(find_cached_disk_image "${MAIN_DISK_PREFIX}" "main branch")
fi

if [[ -z "${CACHED_DISK_NAME}" ]]; then
    ANY_DISK_PREFIX="${DISK_PREFIX}-.+-[0-9a-f]+-v${LOCAL_STATE_VERSION}-${NETWORK}-${DISK_SUFFIX}"
    CACHED_DISK_NAME=$(find_cached_disk_image "${ANY_DISK_PREFIX}" "any branch")
fi

# Handle case where no suitable disk image is found
if [[ -z "${CACHED_DISK_NAME}" ]]; then
    echo "No suitable cached state disk available."
    echo "Expected pattern: ${COMMIT_DISK_PREFIX}"
    echo "Cached state test jobs must depend on the cached state rebuild job."
    exit 1
fi

echo "Selected Disk: ${CACHED_DISK_NAME}"

# Exporting variables for subsequent steps
echo "Exporting variables for subsequent steps..."
echo "cached_disk_name=${CACHED_DISK_NAME}" >> "${GITHUB_OUTPUT}"
echo "STATE_VERSION=${LOCAL_STATE_VERSION}" >> "${GITHUB_ENV}"
echo "CACHED_DISK_NAME=${CACHED_DISK_NAME}" >> "${GITHUB_ENV}"
echo "DISK_OPTION=image=${CACHED_DISK_NAME,}" >> "${GITHUB_ENV}"
