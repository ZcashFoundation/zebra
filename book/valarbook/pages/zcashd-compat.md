# zebra-zcashd-compat

Run Zebra for consensus and P2P, while keeping zcashd for wallet and zcashd-compatible RPC surfaces.

**Status:** Alpha

## Integration Summary

`zebra-zcashd-compat` is a Zebra binary that manages a zcashd child process. Zebra is responsible for consensus and P2P. zcashd is kept for wallet and zcashd-compatible RPC surfaces. zcashd ingests chain and mempool data from Zebra over authenticated RPC. The primary purpose of this integration is to allow exchanges to smoothly migrate to a Zebra node without disrupting their existing zcashd setup.

- **Trust boundary:** zcashd trusts the configured Zebra node for block source and transaction forwarding.
- **Transport:** zcashd talks to Zebra over RPC only. There is no zcashd P2P in compat mode.
- **Auth:** keep Zebra cookie auth enabled and avoid exposing RPC publicly.
- **Deployment:** deploy zcashd and Zebra on the same machine.
- **Lifecycle:** supervised mode manages the zcashd process lifecycle at runtime. For container builds, zcashd artifact fetch and SHA verification happen outside the Dockerfile before image build.

## Prerequisites

Because this setup runs a Zebra node alongside your zcashd wallet API, the hardware requirements are increased. Provision the recommended hardware before restarting from the updated binary.

### Hardware Requirements

- **CPU:** 8 logical CPUs available to the process.
- **RAM:** 32 GiB effective memory.
- **Disk:** at least 1 TiB combined capacity across the filesystems used by Zebra state and zcashd datadir.

Startup fails below the minimums: 4 logical CPUs, 16 GiB RAM, 300 GiB per data volume, or 600 GiB if Zebra state and the zcashd datadir share a filesystem. Bypass only with `--unsafe-low-specs`. Linux x86_64 only.

### Downloads

Binaries:

- [Zebra with zcashd compatibility](https://github.com/valargroup/zebra/releases)

Snapshots:

- [Zebra](https://zebra.valargroup.org/)
- [zcashd](https://zcashd.valargroup.org/)

## Configuration

Choose one of the setup modes below.

### Supervised

Supervised mode is the default path: one Zebra binary. Zebra resolves a hash-pinned compatible zcashd build, starts it as a child process, and supervises restarts.

Start Zebra in zcashd-compat mode:

```bash
zebrad start --zcashd-compat
```

Use this minimal configuration to keep supervision enabled so Zebra manages install and lifecycle:

```text
[zcashd_compat]
enabled = true
manage_zcashd = true
zcashd_source = "managed"

# Optionally, customize zcashd data directory.
zcashd_datadir = "/path/to/zcashd/datadir"

# Optionally, propagate zcashd CLI arguments via env var.
ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-printtoconsole"]'
```

For full configuration reference, see the [full documentation](https://github.com/valargroup/zebra/blob/ironwood-main/book/src/user/zcashd-compat.md).

### Build From Source

Build Zebra and zcashd from source:

```bash
# in zebrad tree
cargo build --release

# in zcashd tree
./zcutil/build.sh -j"$(nproc)"
```

Configure Zebra for manual operation without zcashd supervision:

```toml
[zcashd_compat]
enabled = true
manage_zcashd = false
```

Then run zcashd against Zebra RPC manually:

```bash
zebrad start --zcashd-compat

# Default location, to which Zebra writes an auth cookie
ZEBRA_COMPAT_COOKIE="$HOME/.cache/zebra/.zcashd-compat.cookie"

# Ensure Zebra has written it
test -f "$ZEBRA_COMPAT_COOKIE" && echo "cookie ready: $ZEBRA_COMPAT_COOKIE"

# One-time zcashd datadir setup (zcashd refuses to start without this line)
mkdir -p ~/.zcash
echo 'i-am-aware-zcashd-will-be-replaced-by-zebrad-and-zallet-in-2025=1' >> ~/.zcash/zcash.conf

# Start zcashd in compat mode, pointed at Zebra's compat listener
./src/zcashd -zebra-compat \
  -zebra-compat-url=http://127.0.0.1:28232 \
  -zebra-compat-cookiefile="$HOME/.cache/zebra/.zcashd-compat.cookie" \
  -printtoconsole

# verify
./src/zcash-cli getzebracompatinfo
```

### Docker

Artifacts are fetched and hash-verified externally, then passed in as build context. You can also follow the build-from-source instructions if desired.

Install Docker Buildx and verify it before using the Docker setup mode:

```bash
apt-get update && apt-get install -y docker-buildx && docker buildx version
```

Set the image tag, choose a hash-pinned zcashd artifact for your platform, and define the local build context path:

```bash
export ZEBRA_DOCKER_IMAGE="zebra:zcashd-compat"

# Pick the matching artifact for your host architecture.
# x86_64 example:
export ZCASHD_COMPAT_URL="https://github.com/valargroup/zcashd/releases/download/v6.2.1-alpha/zcashd-zebra-compat-v6.2.1-alpha-linux-x86_64.tar.gz"
export ZCASHD_COMPAT_SHA256="09e640b55c9af91dee5742e5e9bb6712f92d7073f0fe899ca58d43f62eb9d13c"

# Local Docker build context that will contain ./bin/zcashd.
export ZCASHD_COMPAT_CONTEXT="${PWD}/target/zcashd-compat/context"

# State directories for Zebra and zcashd.
export ZEBRA_STATE_CACHE_DIR="/mnt/data/zebra-state"
export ZCASHD_DATADIR="/mnt/data/.zcashd"
```

Download the artifact, verify its SHA256, extract it into a local Docker build context, and confirm the expected binary exists:

```bash
mkdir -p "${ZCASHD_COMPAT_CONTEXT}"
curl -fsSL "${ZCASHD_COMPAT_URL}" -o /tmp/zcashd-compat.tar.gz
echo "${ZCASHD_COMPAT_SHA256}  /tmp/zcashd-compat.tar.gz" | sha256sum -c -
tar -xzf /tmp/zcashd-compat.tar.gz -C "${ZCASHD_COMPAT_CONTEXT}"

# Guardrail: this fails early if ./bin/zcashd is missing or not executable.
test -x "${ZCASHD_COMPAT_CONTEXT}/bin/zcashd"
```

Build the runtime-zcashd-compat target and inject the prepared zcashd binary via `--build-context`:

```bash
docker build \
  -f ./docker/Dockerfile \
  --target runtime-zcashd-compat \
  --build-context "zcashd_compat=${ZCASHD_COMPAT_CONTEXT}" \
  --tag "${ZEBRA_DOCKER_IMAGE}" \
  .
```

Ensure Zebra and zcashd data folders are accessible inside the container:

```bash
# 10001 is the default Dockerfile container user
# Ensure that zebra and zcashd data folders are accessible inside the container
mkdir -p "${ZEBRA_STATE_CACHE_DIR}" "${ZCASHD_DATADIR}"
chown -R 10001:10001 "${ZEBRA_STATE_CACHE_DIR}" "${ZCASHD_DATADIR}"
```

Start Zebra in zcashd compatibility mode with persistent Zebra and zcashd state mounted from the host:

```bash
docker run --rm -it \
  -e ZCASHD_COMPAT_ENABLED=true \
  -e ZEBRA_NETWORK__LISTEN_ADDR="[::]:18233" \
  -e ZEBRA_STATE__CACHE_DIR="/home/zebra/.cache/zebra" \
  -e ZEBRA_ZCASHD_COMPAT__ZCASHD_DATADIR="/home/zebra/.cache/zcashd" \
  -e ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-rpcbind=0.0.0.0","-rpcallowip=0.0.0.0/0"]' \
  --mount type=bind,src="${ZEBRA_STATE_CACHE_DIR}",dst="/home/zebra/.cache/zebra" \
  --mount type=bind,src="${ZCASHD_DATADIR}",dst="/home/zebra/.cache/zcashd" \
  -p 18233:18233 \
  -p 127.0.0.1:8232:8232 \
  "${ZEBRA_DOCKER_IMAGE}" \
  zebrad start --zcashd-compat
```

## FAQ

### How do I preserve my original zcashd configuration?

Point Zebra at your existing zcashd data directory with `zcashd_datadir`, and pass any required zcashd CLI arguments through `zcashd_extra_args`. These settings can be provided in Zebra configuration, or through equivalent runtime overrides where supported. See the [full instructions](https://github.com/valargroup/zebra/blob/ironwood-main/book/src/user/zcashd-compat.md) for details.

Remove peer-directing options from your existing `zcash.conf` first:

- `bind=`
- `whitebind=`
- `connect=`
- `addnode=`
- `seednode=`

Startup fails validation if they are present.

### If my original hardware is below the recommended specification, do I have to provision more resources?

Yes. Because this mode runs Zebra alongside the zcashd wallet API, nodes below the minimum specification fail startup preflight. Between minimum and recommended, Zebra starts with warnings but is not supported for production. Provision hardware that meets the requirements in this guide before restarting with the updated binary.

### I have a restricted setup that does not allow IPC and also restricts networking. What do you recommend?

Build from source and configure the components manually. This gives you direct control over how Zebra and zcashd communicate in your restricted environment.

### How is my old zcashd config managed?

Even if present, the system force-disables the following zcashd config parameters because P2P is unused:

- `p2p`
- `listen`
- `dnsseed`
- `listenonion`

Otherwise, zcashd would fail to start.

### What happens if I do not have a prior config?

A default one with sane defaults is created for you. See the [full instructions](https://github.com/valargroup/zebra/blob/ironwood-main/book/src/user/zcashd-compat.md) for details about location.
