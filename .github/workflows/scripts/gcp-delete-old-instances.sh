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
if ! INSTANCES=$(gcloud compute instances list --sort-by=creationTimestamp --filter="name~-[0-9a-f]{7,}$ AND creationTimestamp < ${DELETE_BEFORE_DATE}" --format='value(NAME,ZONE)' | sed 's/\(.*\)\t\(.*\)/\1 --zone=\2/'); then
    echo "Error fetching instances."
    exit 1
fi

# Delete instances if any are found
if [[ -n "${INSTANCES}" ]]; then
    IFS=$'\n'
    for INSTANCE_AND_ZONE in ${INSTANCES}; do
        IFS=$' '
        echo "Deleting instance: ${INSTANCE_AND_ZONE}"
        gcloud compute instances delete --verbosity=info "${INSTANCE_AND_ZONE}" --delete-disks=all || {
            echo "Failed to delete instance: ${INSTANCE_AND_ZONE}"
            continue
        }
        IFS=$'\n'
    done
    IFS=$' \t\n' # Reset IFS to its default value
else
    echo "No instances to delete."
fi
