#!/bin/bash
#
# Apply Linux sysctl settings that significantly improve full-block propagation
# between Zebra peers.
#
# The most impactful change is disabling TCP slow-start-after-idle. With it
# enabled (the default on most distros), TCP resets the congestion window
# between every idle period, so each block request starts cold. On long-haul
# links this can cap single-peer throughput far below the available bandwidth.
#
# See book/src/user/troubleshooting.md for context and benchmarks.

set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "This script must be run as root (use sudo)." >&2
    exit 1
fi

CONF_FILE="/etc/sysctl.d/99-zebra-network.conf"

echo "Writing $CONF_FILE..."

cat <<EOF > "$CONF_FILE"
# Zebra network tuning
# See https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/troubleshooting.md

# Disable slow-start-after-idle so TCP doesn't reset cwnd between block requests.
# This is the highest-impact setting for full-block propagation on long-haul links.
net.ipv4.tcp_slow_start_after_idle=0

# Use CUBIC congestion control (already the default on most Linux distros;
# set explicitly so the configuration is self-documenting).
net.ipv4.tcp_congestion_control=cubic

# Use fq_codel as the default queueing discipline.
net.core.default_qdisc=fq_codel
EOF

echo "Reloading sysctl..."
sysctl --system >/dev/null

echo
echo "Applied settings:"
sysctl net.ipv4.tcp_slow_start_after_idle \
       net.ipv4.tcp_congestion_control \
       net.core.default_qdisc
