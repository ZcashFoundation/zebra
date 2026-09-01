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

# Fetch the list of instances to delete.
#
# Integration test instances end in the GitHub run id, not a commit hash. Decimal digits
# are a subset of `[0-9a-f]`, so they match this pattern, but only incidentally: do not
# tighten it to assume a hexadecimal commit hash, or those instances will never be reaped.
if ! INSTANCES=$(gcloud compute instances list --sort-by=creationTimestamp --filter="name~-[0-9a-f]{7,}$ AND creationTimestamp < ${DELETE_BEFORE_DATE}" --format='value(NAME,ZONE)'); then
    echo "Error fetching instances."
    exit 1
fi

# Delete each instance.
#
# A failed delete is recorded rather than propagated, so one failure does not abort the
# loop and strand the instances after it - but the script still exits non-zero at the
# end. This sweep is the backstop for the per-run teardown in
# zfnd-deploy-integration-tests-gcp.yml, and it is the last thing that will notice an
# orphan: if a persistent failure (an IAM change, an instance stuck deleting) leaves the
# scheduled run green, nothing else reports the leak.
DELETE_FAILED=0
while IFS=$'\t' read -r NAME ZONE; do
    [[ -z "${NAME}" ]] && continue
    echo "Deleting instance: ${NAME} (--zone=${ZONE})"
    gcloud compute instances delete "${NAME}" --zone="${ZONE}" --delete-disks=all --quiet --verbosity=info \
        || { echo "Failed to delete instance: ${NAME}"; DELETE_FAILED=1; }
done <<< "${INSTANCES}"

if [[ "${DELETE_FAILED}" -ne 0 ]]; then
    echo "ERROR: one or more instances could not be deleted; they are still running"
    exit 1
fi
