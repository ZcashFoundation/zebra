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
# If no category has an image for the current state version, the same search
# runs on the previous state version, which zebrad upgrades in place. The
# disk type availability checks use the same fallback.
#
# In ZcashFoundation/zebra, images written by copies of the repository are
# skipped. Copies share the GCP project and name their images
# `<prefix>-r<repository id>-<sha>-...`.
#
# The selected image is used for setting up the environment in a CI/CD pipeline.
# It also checks if specific disk types are available for subsequent jobs.

set -eo pipefail

# Extract local state version
echo "Extracting local state version..."
LOCAL_STATE_VERSION=$(grep -oE "DATABASE_FORMAT_VERSION: .* [0-9]+" "${GITHUB_WORKSPACE}/zebra-state/src/constants.rs" | grep -oE "[0-9]+" | tail -n1)
echo "STATE_VERSION: ${LOCAL_STATE_VERSION}"

# zebrad only reuses a database from the immediately previous major version, and
# only when the current major version has a format upgrade named `<major>.0.0`.
# Otherwise it would start from an empty database, so there is no fallback.
UPGRADES_DIR="${GITHUB_WORKSPACE}/zebra-state/src/service/finalized_state/disk_format"
STATE_VERSIONS=("${LOCAL_STATE_VERSION}")
if grep -rqF "Version::new(${LOCAL_STATE_VERSION}, 0, 0)" "${UPGRADES_DIR}/upgrade.rs" "${UPGRADES_DIR}/upgrade"; then
    STATE_VERSIONS+=("$((LOCAL_STATE_VERSION - 1))")
    echo "PREVIOUS_STATE_VERSION: ${STATE_VERSIONS[1]}"
else
    echo "No previous state version fallback: no ${LOCAL_STATE_VERSION}.0.0 format upgrade."
fi

# Function to print the newest ready image named `<image_prefix>-<name_pattern>`.
# GCE filters use RE2, which has no negative lookahead, so images from copies of
# the repository are dropped from the listed names instead.
find_newest_image() {
    local image_prefix="${1}"
    local name_pattern="${2}"
    local images

    images=$(gcloud compute images list --filter="status=READY AND name~${image_prefix}-${name_pattern}" --format="value(NAME)" --sort-by=~creationTimestamp)

    if [[ "${GITHUB_REPOSITORY}" == "ZcashFoundation/zebra" ]]; then
        grep -m1 -vE -- "^${image_prefix}-r[0-9]+-[0-9a-f]+-v[0-9]+-" <<<"${images}" || true
    else
        head -n1 <<<"${images}"
    fi
}

# Function to find a cached disk image based on the git pattern (branch, main, or any branch)
find_cached_disk_image() {
    local git_pattern="${1}"
    local git_source="${2}"
    local state_version="${3}"
    local disk_name
    local disk_search_pattern="${git_pattern}-v${state_version}-${NETWORK}-${DISK_SUFFIX}"

    disk_name=$(find_newest_image "${DISK_PREFIX}" "${disk_search_pattern}")

    # Use >&2 to redirect to stderr and avoid sending wrong assignments to stdout
    if [[ -n "${disk_name}" ]]; then
        echo "Found ${git_source} Disk: ${disk_name}" >&2
        disk_description=$(gcloud compute images describe "${disk_name}" --format="value(DESCRIPTION)")
        echo "Description: ${disk_description}" >&2
        echo "${disk_name}" # This is the actual return value when a disk is found
    else
        echo "No ${git_source} disk found with '${DISK_PREFIX}-${disk_search_pattern}' pattern." >&2
    fi
}

# Check if both $DISK_PREFIX and $DISK_SUFFIX are set, as they are required to
# find a cached disk image.
if [[ -n "${DISK_PREFIX}" && -n "${DISK_SUFFIX}" ]]; then
    # Find the most suitable cached disk image
    echo "Finding a ${DISK_PREFIX}-${DISK_SUFFIX} disk image for ${NETWORK}..."
    CACHED_DISK_NAME=""

    # Search the current state version first, then the previous one.
    for state_version in "${STATE_VERSIONS[@]}"; do
        # Try to find one from the current branch (or PR).
        CACHED_DISK_NAME=$(find_cached_disk_image ".+-${CACHE_SCOPE}" "branch" "${state_version}")
        # If no image was found, try to find an image based on the `main` branch.
        CACHED_DISK_NAME=${CACHED_DISK_NAME:-$(find_cached_disk_image "main-[0-9a-f]+" "main branch" "${state_version}")}
        # If we still have no image, try to find one from any branch.
        CACHED_DISK_NAME=${CACHED_DISK_NAME:-$(find_cached_disk_image ".+-[0-9a-f]+" "any branch" "${state_version}")}

        if [[ -n "${CACHED_DISK_NAME}" ]]; then
            if [[ "${state_version}" != "${LOCAL_STATE_VERSION}" ]]; then
                echo "::warning::No v${LOCAL_STATE_VERSION} cached state image exists. Using the v${state_version} image ${CACHED_DISK_NAME}, which zebrad upgrades in place."
            fi
            break
        fi
    done

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
    local state_version
    local disk_name

    # Use >&2 to redirect to stderr and avoid sending wrong assignments to stdout
    for state_version in "${STATE_VERSIONS[@]}"; do
        disk_name=$(find_newest_image "${base_name}-cache" ".+-[0-9a-f]+-v${state_version}-${NETWORK}-${disk_type}")

        if [[ -n "${disk_name}" ]]; then
            echo "Found ${disk_type^^} disk: ${disk_name} for ${base_name^^} on network: ${NETWORK} (state version ${state_version})" >&2
            disk_description=$(gcloud compute images describe "${disk_name}" --format="value(DESCRIPTION)")
            echo "Description: ${disk_description}" >&2
            echo "true" # This is the actual return value when a disk is found
            return
        fi
    done

    echo "No ${disk_type^^} disk found for ${base_name^^} on network: ${NETWORK}" >&2
    echo "false" # This is the actual return value when no disk is found
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
