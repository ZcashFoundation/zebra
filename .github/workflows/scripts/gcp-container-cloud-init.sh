#!/usr/bin/env bash
#
# Print a cloud-init document that runs one container under systemd on a
# Container-Optimized OS VM, in place of the deprecated container startup agent
# (konlet, `gce-container-declaration`). Pass the output to `gcloud compute
# instances create` or `instance-templates create` as
# `--metadata-from-file=user-data=<file>`.
#
# Configured through environment variables, so callers never splice workflow
# expressions into the document:
#
#   CONTAINER_IMAGE    image to run (required)
#   CONTAINER_NAME     container name, also used for the unit and env file (required)
#   CONTAINER_ENV      comma-separated KEY=VALUE list, as --container-env took (required)
#   DISK_DEVICE_NAME   device name of the data disk, formatted if blank (required)
#   MOUNT_PATHS        space-separated container paths the data disk is mounted at (required)
#   RESTART_POLICY     systemd Restart= value (default: no)
#
# konlet behaviours the document reproduces explicitly:
#   - it formatted an unformatted data disk (`blkid || mkfs.ext4` below)
#   - it named the container `klt-<instance>` (now CONTAINER_NAME)
#   - --container-restart-policy (now RESTART_POLICY)
#   - --container-tty (now `--tty`)
#
# The host mount point deliberately contains no hyphen. A path such as
# /mnt/disks/zebra-cache needs a \x2d escape in the systemd unit name, and that
# backslash does not survive cloud-init's runcmd shell: the mount then fails to
# enable while the container still starts, silently using the boot disk instead
# of the cache disk.

set -euo pipefail

: "${CONTAINER_IMAGE:?}" "${CONTAINER_NAME:?}" "${CONTAINER_ENV:?}"
: "${DISK_DEVICE_NAME:?}" "${MOUNT_PATHS:?}"
RESTART_POLICY="${RESTART_POLICY:-no}"

HOST_CACHE_DIR=/mnt/disks/zebracache
DISK="/dev/disk/by-id/google-${DISK_DEVICE_NAME}"

# The disk is mounted once on the host and bind-mounted at each container path.
DOCKER_MOUNTS=""
for path in ${MOUNT_PATHS}; do
  DOCKER_MOUNTS+="${DOCKER_MOUNTS:+ }-v ${HOST_CACHE_DIR}:${path}"
done

# docker reads the env file as one KEY=VALUE per line.
ENV_FILE_BODY=$(echo "${CONTAINER_ENV}" | tr ',' '\n' | sed 's/^/      /')

cat <<CLOUDINIT
#cloud-config
bootcmd:
- mkdir -p ${HOST_CACHE_DIR}

write_files:
- path: /etc/${CONTAINER_NAME}.env
  permissions: "0600"
  owner: root
  content: |
${ENV_FILE_BODY}

- path: /etc/systemd/system/format-zebracache.service
  permissions: "0644"
  owner: root
  content: |
    [Unit]
    Description=Format the cache disk if it has no filesystem
    Before=mnt-disks-zebracache.mount
    ConditionPathExists=${DISK}

    [Service]
    Type=oneshot
    RemainAfterExit=yes
    # A disk restored from a cached-state image is already formatted, so
    # blkid short-circuits and existing state is never touched.
    ExecStart=/bin/sh -c 'blkid ${DISK} || mkfs.ext4 -F ${DISK}'

    [Install]
    WantedBy=multi-user.target

- path: /etc/systemd/system/mnt-disks-zebracache.mount
  permissions: "0644"
  owner: root
  content: |
    [Unit]
    Description=zebrad cache disk
    After=format-zebracache.service
    Requires=format-zebracache.service
    Before=${CONTAINER_NAME}.service

    [Mount]
    What=${DISK}
    Where=${HOST_CACHE_DIR}
    Type=ext4
    # nofail drops the implicit Before=local-fs.target. Without it the boot
    # transaction has a cycle: local-fs.target, this mount, the format service
    # and sysinit.target. A failed mount still stops the container, which
    # Requires= it.
    Options=discard,defaults,nofail

    [Install]
    WantedBy=multi-user.target

- path: /etc/systemd/system/${CONTAINER_NAME}.service
  permissions: "0644"
  owner: root
  content: |
    [Unit]
    Description=zebrad container
    # Requires (not just RequiresMountsFor) so a failed mount stops the
    # container outright rather than letting it run against the boot disk.
    Requires=docker.service mnt-disks-zebracache.mount
    After=docker.service network-online.target mnt-disks-zebracache.mount

    [Service]
    Type=exec
    Restart=${RESTART_POLICY}
    # konlet inherited docker's 10s SIGKILL, which can truncate zebrad's
    # RocksDB flush. A unit can wait for it.
    TimeoutStopSec=180
    ExecStartPre=-/usr/bin/docker rm -f ${CONTAINER_NAME}
    # Deliberately no --rm: callers read the container's logs after it exits,
    # and a restart replaces it through the ExecStartPre above.
    #
    # --tty is not cosmetic: a TTY merges stderr into stdout, and log-parsing
    # steps read 'docker logs' stdout only, while zebrad logs to stderr. No
    # --interactive: nothing reads stdin, and it fails under systemd without a
    # terminal.
    ExecStart=/usr/bin/docker run --name ${CONTAINER_NAME} \\
      --tty \\
      --network host \\
      --env-file /etc/${CONTAINER_NAME}.env \\
      ${DOCKER_MOUNTS} \\
      ${CONTAINER_IMAGE}
    ExecStop=/usr/bin/docker stop -t 170 ${CONTAINER_NAME}

    [Install]
    WantedBy=multi-user.target

runcmd:
- systemctl daemon-reload
- systemctl enable --now format-zebracache.service
- systemctl enable --now mnt-disks-zebracache.mount
- systemctl enable --now ${CONTAINER_NAME}.service
CLOUDINIT
