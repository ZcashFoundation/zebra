#!/usr/bin/env bash

# This script finds a cached Google Cloud Compute image based on specific criteria.
#
# If there are multiple disks:
# - try to find a cached disk image from the current branch (or PR),
# - if no image was found, try to find an image from the `main` branch,
# - if no image was found, try to find an image from any branch.
#
# Within each of these categories:
# - prefer newer images to older images
#
# The selected image is used for setting up the environment in a CI/CD pipeline.
# It also checks if specific disk types are available for subsequent jobs.

set -eo pipefail

# Extract local state version
echo "Extracting local state version..."
LOCAL_STATE_VERSION=$(grep -oE "DATABASE_FORMAT_VERSION: .* [0-9]+" "${GITHUB_WORKSPACE}/zebra-state/src/constants.rs" | grep -oE "[0-9]+" | tail -n1)
echo "STATE_VERSION: ${LOCAL_STATE_VERSION}"

# Function to find a cached disk image based on the git pattern (branch, main, or any branch)
find_cached_disk_image() {
    local git_pattern="${1}"
    local git_source="${2}"
    local disk_name
    local disk_search_pattern="${DISK_PREFIX}-${git_pattern}-v${LOCAL_STATE_VERSION}-${NETWORK}-${DISK_SUFFIX}"

    disk_name=$(gcloud compute images list --filter="status=READY AND name~${disk_search_pattern}" --format="value(NAME)" --sort-by=~creationTimestamp --limit=1)

    # Use >&2 to redirect to stderr and avoid sending wrong assignments to stdout
    if [[ -n "${disk_name}" ]]; then
        echo "Found ${git_source} Disk: ${disk_name}" >&2
        disk_description=$(gcloud compute images describe "${disk_name}" --format="value(DESCRIPTION)")
        echo "Description: ${disk_description}" >&2
        echo "${disk_name}" # This is the actual return value when a disk is found
    else
        echo "No ${git_source} disk found with '${disk_search_pattern}' pattern." >&2
    fi
}

# Check if both $DISK_PREFIX and $DISK_SUFFIX are set, as they are required to
# find a cached disk image.
if [[ -n "${DISK_PREFIX}" && -n "${DISK_SUFFIX}" ]]; then
    # Find the most suitable cached disk image
    echo "Finding a ${DISK_PREFIX}-${DISK_SUFFIX} disk image for ${NETWORK}..."
    CACHED_DISK_NAME=""

    # Try to find one from the current branch (or PR).
    CACHED_DISK_NAME=$(find_cached_disk_image ".+-${GITHUB_REF}" "branch")
    # If no image was found, try to find an image based on the `main` branch.
    CACHED_DISK_NAME=${CACHED_DISK_NAME:-$(find_cached_disk_image "main-[0-9a-f]+" "main branch")}
    # If we still have no image, try to find one from any branch.
    CACHED_DISK_NAME=${CACHED_DISK_NAME:-$(find_cached_disk_image ".+-[0-9a-f]+" "any branch")}

    # Handle the case where no suitable disk image is found
    if [[ -z "${CACHED_DISK_NAME}" ]]; then
        echo "No suitable cached state disk available. Try running the cached state rebuild job."
        exit 1
    else
        echo "Selected Disk: ${CACHED_DISK_NAME}"
    fi

else
    echo "DISK_PREFIX or DISK_SUFFIX is not set. Skipping disk image search."
fi

# Function to find and output available disk image types (e.g., lwd_tip_disk, zebra_tip_disk, zebra_checkpoint_disk)
find_available_disk_type() {
    local base_name="${1}"
    local disk_type="${2}"
    local disk_pattern="${base_name}-cache"
    local disk_name

    disk_name=$(gcloud compute images list --filter="status=READY AND name~${disk_pattern}-.+-[0-9a-f]+-v${LOCAL_STATE_VERSION}-${NETWORK}-${disk_type}" --format="value(NAME)" --sort-by=~creationTimestamp --limit=1)

    # Use >&2 to redirect to stderr and avoid sending wrong assignments to stdout
    if [[ -n "${disk_name}" ]]; then
        echo "Found ${disk_type^^} disk: ${disk_name} for ${base_name^^} on network: ${NETWORK}" >&2
        disk_description=$(gcloud compute images describe "${disk_name}" --format="value(DESCRIPTION)")
        echo "Description: ${disk_description}" >&2
        echo "true" # This is the actual return value when a disk is found
    else
        echo "No ${disk_type^^} disk found for ${base_name^^} on network: ${NETWORK}" >&2
        echo "false" # This is the actual return value when no disk is found
    fi
}
if [[ -n "${NETWORK}" ]]; then
    # Check for specific disk images (lwd_tip_disk, zebra_tip_disk, zebra_checkpoint_disk)
    echo "Checking for specific disk images..."
    LWD_TIP_DISK=$(find_available_disk_type "lwd" "tip")
    ZEBRA_TIP_DISK=$(find_available_disk_type "zebrad" "tip")
    ZEBRA_CHECKPOINT_DISK=$(find_available_disk_type "zebrad" "checkpoint")
fi

# Exporting variables for subsequent steps
echo "Exporting variables for subsequent steps..."
export CACHED_DISK_NAME="${CACHED_DISK_NAME}"
export LOCAL_STATE_VERSION="${LOCAL_STATE_VERSION}"
export LWD_TIP_DISK="${LWD_TIP_DISK}"
export ZEBRA_TIP_DISK="${ZEBRA_TIP_DISK}"
export ZEBRA_CHECKPOINT_DISK="${ZEBRA_CHECKPOINT_DISK}"
