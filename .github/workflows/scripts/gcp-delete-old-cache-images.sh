#!/usr/bin/env bash

# Function to handle image deletion logic
delete_images() {
    local image_type="$1"
    local filter="$2"
    local kept_images=0

    echo "Processing ${image_type} images"
    images=$(gcloud compute images list --sort-by=~creationTimestamp --filter="${filter} AND creationTimestamp < ${DELETE_BEFORE_DATE}" --format='value(NAME)')

    for image in ${images}; do
        if [[ "${kept_images}" -lt "${KEEP_LATEST_IMAGE_COUNT}" ]]; then
            ((kept_images++))
            echo "Keeping image ${kept_images} named ${image}"
        else
            echo "Deleting image: ${image}"
            gcloud compute images delete "${image}" || echo "Failed to delete image: ${image}"
        fi
    done
}

# Check if necessary variables are set
if ! [[ "${DELETE_AGE_DAYS}" =~ ^[0-9]+$ && "${KEEP_LATEST_IMAGE_COUNT}" =~ ^[0-9]+$ ]]; then
    echo "ERROR: One or more required variables are not set or not numeric"
    exit 1
fi

# Set pipefail
set -o pipefail

# Calculate the date before which images should be deleted
DELETE_BEFORE_DATE=$(date --date="${DELETE_AGE_DAYS} days ago" '+%Y%m%d')

# Mainnet and Testnet zebrad checkpoint
delete_images "Mainnet zebrad checkpoint" "name~^zebrad-cache-.*-mainnet-checkpoint" # As of April 2023, these disk names look like: zebrad-cache-6556-merge-a2ca4de-v25-mainnet-tip(-u)?-140654
delete_images "Testnet zebrad checkpoint" "name~^zebrad-cache-.*-testnet-checkpoint"

# Mainnet and Testnet zebrad tip
delete_images "Mainnet zebrad tip" "name~^zebrad-cache-.*-mainnet-tip"
delete_images "Testnet zebrad tip" "name~^zebrad-cache-.*-testnet-tip"

# Mainnet and Testnet lightwalletd tip
delete_images "Mainnet lightwalletd tip" "name~^lwd-cache-.*-mainnet-tip"
delete_images "Testnet lightwalletd tip" "name~^lwd-cache-.*-testnet-tip"
