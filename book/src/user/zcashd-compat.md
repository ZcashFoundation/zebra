# zcashd-compat Mode (`zebrad start --zcashd-compat`)

zcashd-compat mode runs Zebra as the consensus source and optionally supervises a
`zcashd -zebra-compat` child process that uses Zebra's RPC endpoint for chain data,
mempool data, and transaction forwarding.

## What zcashd-compat mode does

When you start Zebra with:

```console
zebrad start --zcashd-compat
```

Zebra:

- enables zcashd-compat mode (`[zcashd_compat].enabled = true`);
- ensures a dedicated zcashd-compat RPC listen address is configured (defaults to `127.0.0.1:28232`);
- uses dedicated cookie auth at `zcashd_compat.cookie_dir/zcashd_compat.cookie_file_name`;
- raises the zcashd-compat RPC `max_response_body_size` if needed for large batched block responses;
- optionally spawns and supervises `zcashd -zebra-compat`.

If zcashd-compat supervision is enabled, Zebra starts `zcashd` with:

```text
-zebra-compat
-zebra-compat-url=<rpc_url>
-zebra-compat-cookiefile=<zcashd_compat.cookie_dir>/<zcashd_compat.cookie_file_name>
-datadir=<zcashd_compat.zcashd_datadir or state.cache_dir/zcashd-compat-zcashd>
[-testnet | -regtest]
```

## Configuration

zcashd-compat mode adds a `[zcashd_compat]` section:

```toml
[zcashd_compat]
enabled = false
manage_zcashd = true
zcashd_source = "managed"                            # "managed" or "path"
zcashd_path = "/path/to/local/zcashd"               # optional explicit override
zcashd_datadir = "/path/to/zcashd/datadir"          # optional
zcashd_extra_args = ["-debug=1"]                    # optional extra args
listen_addr = "127.0.0.1:28232"                     # optional, default set when zcashd-compat is enabled
cookie_dir = "/path/to/cookies"                     # optional, defaults to <cache_dir>
cookie_file_name = ".zcashd-compat.cookie"          # optional, defaults to ".zcashd-compat.cookie"
startup_delay = "1s"
restart_backoff = "2s"
max_restarts = 10
shutdown_grace_period = "10s"
```

When overriding `zcashd_extra_args` via environment variables, pass a JSON array string:

```console
ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-conf=/path/to/zcash.conf","-debug=1"]'
```

`zebrad` always adds `-printtoconsole` automatically for supervised `zcashd`.

## Hardware preflight (Linux)

When zcashd-compat mode is enabled, Zebra runs Linux-only startup preflight checks
for CPU, effective RAM, and mount-aware provisioned disk space.

If zcashd-compat mode is enabled on a non-Linux host, Zebra fails startup by
default because zcashd-compat is currently Linux-only. You can explicitly bypass
this guardrail with `--unsafe-low-specs`.

If hardware is below minimum requirements, Zebra fails closed by default.
If hardware is below recommended requirements but above minimums, Zebra logs
explicit warnings and continues.

Use `--unsafe-low-specs` to bypass minimum-check failures only when you
explicitly accept degraded or unstable operation.

### Minimum requirements (fail closed by default)

- CPU: 4 logical CPUs available to the process
- RAM: 16 GiB effective memory (host memory, constrained by cgroup limits when applicable)
- Disk:
  - Zebra state mount: at least 300 GiB provisioned capacity
  - zcashd datadir mount: at least 300 GiB provisioned capacity
  - If Zebra state and zcashd datadir are on the same filesystem, required
    provisioned capacity is summed (600 GiB provisioned)

### Recommended requirements (warn if below)

- CPU: 8 logical CPUs available to the process
- RAM: 32 GiB effective memory
- Disk: at least 1 TiB combined capacity across the filesystems used by Zebra state
  and zcashd datadir

## Containers

The standard container image does not enable zcashd-compat or include `zcashd`
by default. Release builds publish a separate `zfnd/zebra-zcashd-compat` image,
or you can build a local compat image with:

```console
make compat-docker-build
```

`make compat-docker-build` downloads a hash-pinned zcashd-compat archive,
verifies its SHA256, and passes the extracted `zcashd` binary into the Docker
build using a named BuildKit context. This keeps network fetching outside the
Dockerfile and lets callers supply their own binary source.

To override the default source, set `ZCASHD_COMPAT_BUILD_CONTEXT` to a local
directory that contains `./bin/zcashd`:

```console
make compat-docker-build \
  ZCASHD_COMPAT_BUILD_CONTEXT=/path/to/extracted-zcashd-context
```

At runtime, enable the vendored binary with:

```console
ZCASHD_COMPAT_ENABLED=true
```

The container entrypoint does not set zcashd-compat config by default. If this
environment variable is set and `/usr/local/bin/zcashd` exists, the entrypoint
enables zcashd-compat mode and configures Zebra to use that local binary.

If `manage_zcashd = false`, Zebra still applies zcashd-compat RPC guardrails, but
does not spawn `zcashd`.

The standard `[rpc]` listener remains independent. zcashd-compat uses a separate
listener and separate cookie auth so operators can keep user-facing Zebra RPC
and zcashd backend RPC isolated.

If `manage_zcashd = true`, Zebra resolves `zcashd` as follows:

1. If `zcashd_path` is set, Zebra uses that local executable directly.
2. Otherwise, `zcashd_source = "managed"` uses Zebra's embedded release manifest
   to fetch a compatible `zcashd` archive, verify its SHA256, cache it, and run it.
3. `zcashd_source = "path"` requires `zcashd_path` to be set.

Managed downloads are cached under:

```text
<state.cache_dir>/zcashd-compat/bin/<release_tag>/<target>/zcashd
```

If managed artifacts are unavailable for the local platform, set `zcashd_path`
to a local binary instead.

## Quick regtest loop

1. Configure:

```toml
[network]
network = "Regtest"
```

1. Start Zebra in zcashd-compat mode:

```console
zebrad start --zcashd-compat
```

1. Confirm the startup log banner shows the zcashd-compat RPC URL and cookie file.

2. Use `zcash-cli getzebracompatinfo` to verify Zebra identity and readiness.

3. Generate blocks via Zebra RPC (`generate`) and verify `zcashd` follows.

## Notes

- `zcashd -zebra-compat` talks to Zebra over RPC, not over peer-to-peer connections.
- On shutdown, Zebra sends SIGTERM to the supervised `zcashd`, then SIGKILL
  after `shutdown_grace_period` if needed.
- Regtest interoperability can depend on matching assumptions between Zebra and
  `zcashd` builds. If regtest semantics diverge, use testnet for initial
  interoperability validation.

## Process lifecycle behavior

When zcashd-compat supervision is enabled (`zcashd_compat.enabled = true` and
`zcashd_compat.manage_zcashd = true`):

- If `zcashd` exits unexpectedly, Zebra's zcashd-compat supervisor restarts it using
  `restart_backoff`, up to `max_restarts`.
- If the zcashd-compat supervisor later exits or returns a runtime error (for
  example, spawn failures or restart-limit exhaustion while running), Zebra logs
  a warning and keeps running without zcashd supervision.
- Startup-time zcashd-compat config validation is unchanged. For example, if
  `zcashd_compat.manage_zcashd = true` and explicit `zcashd_path` cannot be resolved,
  Zebra startup fails with an error.
- Managed download failures (missing target artifact, hash mismatch, transport
  failures) fail closed before Zebra supervises `zcashd`.
- If `zebrad` is shut down normally, it asks the zcashd-compat supervisor to stop
  `zcashd` gracefully: SIGTERM first, then SIGKILL after
  `shutdown_grace_period` if needed.
- If `zebrad` is terminated ungracefully (for example `kill -9`), normal
  shutdown handlers do not run, so `zcashd` can remain running until it is
  stopped externally.
