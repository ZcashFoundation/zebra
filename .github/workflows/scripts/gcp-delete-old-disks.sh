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

# Fetch disks created by PR jobs, and other jobs that use a commit hash
if ! COMMIT_DISKS=$(gcloud compute disks list --sort-by=creationTimestamp --filter="name~-[0-9a-f]{7,}$ AND creationTimestamp < ${DELETE_BEFORE_DATE}" --format='value(NAME,LOCATION,LOCATION_SCOPE)' | sed 's/\(.*\)\t\(.*\)\t\(.*\)/\1 --\3=\2/'); then
    echo "Error fetching COMMIT_DISKS."
    exit 1
fi

# Delete commit disks if any are found
IFS=$'\n'
for DISK_AND_LOCATION in ${COMMIT_DISKS}; do
    IFS=$' '
    echo "Deleting disk: ${DISK_AND_LOCATION}"
    if ! gcloud compute disks delete --verbosity=info "${DISK_AND_LOCATION}"; then
        echo "Failed to delete disk: ${DISK_AND_LOCATION}"
    fi
    IFS=$'\n'
done
IFS=$' \t\n' # Reset IFS to its default value

# Fetch disks created by managed instance groups, and other jobs that start with "zebrad-"
if ! ZEBRAD_DISKS=$(gcloud compute disks list --sort-by=creationTimestamp --filter="name~^zebrad- AND creationTimestamp < ${DELETE_BEFORE_DATE}" --format='value(NAME,LOCATION,LOCATION_SCOPE)' | sed 's/\(.*\)\t\(.*\)\t\(.*\)/\1 --\3=\2/'); then
    echo "Error fetching ZEBRAD_DISKS."
    exit 1
fi

# Delete zebrad disks if any are found
IFS=$'\n'
for DISK_AND_LOCATION in ${ZEBRAD_DISKS}; do
    IFS=$' '
    echo "Deleting disk: ${DISK_AND_LOCATION}"
    if ! gcloud compute disks delete --verbosity=info "${DISK_AND_LOCATION}"; then
        echo "Failed to delete disk: ${DISK_AND_LOCATION}"
    fi
    IFS=$'\n'
done
IFS=$' \t\n' # Reset IFS to its default value
