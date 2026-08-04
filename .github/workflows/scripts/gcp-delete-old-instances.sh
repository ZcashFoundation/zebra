#!/usr/bin/env bash

# Check if DELETE_INSTANCE_DAYS is set and is a number
if ! [[ "${DELETE_INSTANCE_DAYS}" =~ ^[0-9]+$ ]]; then
    echo "ERROR: DELETE_INSTANCE_DAYS is not set or not a number"
    exit 1
fi

# Set pipefail to catch errors in pipelines
set -o pipefail

# Calculate the date before which instances should be deleted
DELETE_BEFORE_DATE=$(date --date="${DELETE_INSTANCE_DAYS} days ago" '+%Y%m%d')

# Check if gcloud command is available
if ! command -v gcloud &> /dev/null; then
    echo "ERROR: gcloud command not found"
    exit 1
fi

# Fetch the list of instances to delete
if ! INSTANCES=$(gcloud compute instances list --sort-by=creationTimestamp --filter="name~-[0-9a-f]{7,}$ AND creationTimestamp < ${DELETE_BEFORE_DATE}" --format='value(NAME,ZONE)'); then
    echo "Error fetching instances."
    exit 1
fi

# Delete each instance
while IFS=$'\t' read -r NAME ZONE; do
    [[ -z "${NAME}" ]] && continue
    echo "Deleting instance: ${NAME} (--zone=${ZONE})"
    gcloud compute instances delete "${NAME}" --zone="${ZONE}" --delete-disks=all --quiet --verbosity=info \
        || echo "Failed to delete instance: ${NAME}"
done <<< "${INSTANCES}"
