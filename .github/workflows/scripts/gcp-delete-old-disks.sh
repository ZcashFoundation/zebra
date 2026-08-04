#!/usr/bin/env bash

# Check if DELETE_AGE_DAYS is set and is a number
if ! [[ "${DELETE_AGE_DAYS}" =~ ^[0-9]+$ ]]; then
    echo "ERROR: DELETE_AGE_DAYS is not set or not a number"
    exit 1
fi

# Set pipefail to catch errors in pipelines
set -o pipefail

# Calculate the date before which disks should be deleted
DELETE_BEFORE_DATE=$(date --date="${DELETE_AGE_DAYS} days ago" '+%Y%m%d')

# Deletes old, unattached disks matching the gcloud name filter in $1.
delete_disks() {
    local disks
    if ! disks=$(gcloud compute disks list --sort-by=creationTimestamp --filter="$1 AND creationTimestamp < ${DELETE_BEFORE_DATE} AND -users:*" --format='value(NAME,LOCATION,LOCATION_SCOPE)'); then
        echo "Error listing disks for filter: $1"
        exit 1
    fi
    while IFS=$'\t' read -r NAME LOCATION SCOPE; do
        [[ -z "${NAME}" ]] && continue
        echo "Deleting disk: ${NAME} (--${SCOPE}=${LOCATION})"
        gcloud compute disks delete "${NAME}" "--${SCOPE}=${LOCATION}" --quiet --verbosity=info \
            || echo "Failed to delete disk: ${NAME}"
    done <<< "${disks}"
}

# Disks from PR jobs and other jobs that use a commit hash.
delete_disks 'name~-[0-9a-f]{7,}$'
# Disks prefixed with "zebrad-", but never the persistent "zebrad-cache-*" chain-state disks.
delete_disks 'name~^zebrad- AND NOT name~^zebrad-cache'
