#!/usr/bin/env bash

# Check if DELETE_AGE_DAYS is set and is a number
if ! [[ "${DELETE_AGE_DAYS}" =~ ^[0-9]+$ ]]; then
    echo "ERROR: DELETE_AGE_DAYS is not set or not a number"
    exit 1
fi

# Set pipefail to catch errors in pipelines
set -o pipefail

# Calculate the date before which templates should be deleted
DELETE_BEFORE_DATE=$(date --date="${DELETE_AGE_DAYS} days ago" '+%Y%m%d')

# Check if gcloud command is available
if ! command -v gcloud &> /dev/null; then
    echo "ERROR: gcloud command not found"
    exit 1
fi

# Fetch the list of instance templates to delete
if ! TEMPLATES=$(gcloud compute instance-templates list --sort-by=creationTimestamp --filter="name~-[0-9a-f]{7,}$ AND creationTimestamp < ${DELETE_BEFORE_DATE}" --format='value(NAME)'); then
    echo "Error fetching instance templates."
    exit 1
fi

# Delete templates if any are found
for TEMPLATE in ${TEMPLATES}; do
    echo "Deleting template: ${TEMPLATE}"
    if ! gcloud compute instance-templates delete "${TEMPLATE}"; then
        echo "Failed to delete template: ${TEMPLATE}"
    fi
done
