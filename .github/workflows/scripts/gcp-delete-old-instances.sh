#!/usr/bin/env bash

# Check that the retention thresholds are set and are numbers
if ! [[ "${DELETE_INSTANCE_DAYS}" =~ ^[0-9]+$ ]]; then
    echo "ERROR: DELETE_INSTANCE_DAYS is not set or not a number"
    exit 1
fi
if ! [[ "${DELETE_LONG_TEST_INSTANCE_DAYS}" =~ ^[0-9]+$ ]]; then
    echo "ERROR: DELETE_LONG_TEST_INSTANCE_DAYS is not set or not a number"
    exit 1
fi

# Set pipefail to catch errors in pipelines
set -o pipefail

# Check if gcloud command is available
if ! command -v gcloud &> /dev/null; then
    echo "ERROR: gcloud command not found"
    exit 1
fi

# Deletes instances matching the gcloud name/label filter in $1 that were
# created more than $2 days ago.
delete_instances() {
    local filter="$1"
    local days="$2"
    local before instances
    before=$(date --date="${days} days ago" '+%Y%m%d')

    if ! instances=$(gcloud compute instances list --sort-by=creationTimestamp --filter="${filter} AND creationTimestamp < ${before}" --format='value(NAME,ZONE)'); then
        echo "Error fetching instances for filter: ${filter}"
        exit 1
    fi
    while IFS=$'\t' read -r NAME ZONE; do
        [[ -z "${NAME}" ]] && continue
        echo "Deleting instance: ${NAME} (--zone=${ZONE})"
        gcloud compute instances delete "${NAME}" --zone="${ZONE}" --delete-disks=all --quiet --verbosity=info \
            || echo "Failed to delete instance: ${NAME}"
    done <<< "${instances}"
}

# Integration test instances end in the GitHub run id, not a commit hash. Decimal digits
# are a subset of `[0-9a-f]`, so they match this pattern, but only incidentally: do not
# tighten it to assume a hexadecimal commit hash, or those instances will never be reaped.
#
# Instances labelled `long-test=true` run tests whose job timeout is longer than the
# default retention (see `is_long_test` in zfnd-deploy-integration-tests-gcp.yml), so
# they get their own threshold: reaping them at the default one would delete the VM and
# its state disk mid-test, days into a sync. An instance without the label falls only
# under the first tier - `NOT labels.long-test=true` also matches instances that don't
# carry the label at all.
delete_instances "name~-[0-9a-f]{7,}$ AND NOT labels.long-test=true" "${DELETE_INSTANCE_DAYS}"
delete_instances "name~-[0-9a-f]{7,}$ AND labels.long-test=true" "${DELETE_LONG_TEST_INSTANCE_DAYS}"
