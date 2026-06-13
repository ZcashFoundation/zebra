# zcashd-compat Mode (`zebrad start --zcashd-compat`)

zcashd-compat mode is for operators — typically exchanges and custodial
services — that want to migrate to Zebra while keeping the `zcashd` wallet and
RPC surface their integration already depends on. Zebra becomes the consensus
node: it syncs the Zcash network over P2P, and a `zcashd` running with P2P
disabled ingests chain data, mempool data, and transaction forwarding from
Zebra over JSON-RPC.

Your systems keep talking to `zcashd` exactly as before:

| Provided by `zcashd`, unchanged          | Moved to Zebra                              |
|------------------------------------------|---------------------------------------------|
| Wallet behavior and wallet RPC methods   | P2P networking and peer selection           |
| Local block files, chainstate, indexes   | Block acquisition and best-chain selection  |
| ZMQ notifications                        | Transaction relay to the network            |
| Local RPC response semantics             | Expensive cryptographic block verification  |

```text
Zcash network ═P2P═▶ zebrad ◀─JSON-RPC (cookie auth, optional TLS)─ zcashd -zebra-compat ◀─wallet RPC, ZMQ─ your systems
```

There are two ways to run the pair:

- **Externally managed (default):** you run `zcashd -zebra-compat` yourself
  against Zebra's zcashd-compat RPC endpoint.
- **Supervised:** Zebra spawns and manages `zcashd` itself when
  `[zcashd_compat].manage_zcashd = true`.

The zcashd-side flags, trust model, sizing arithmetic, diagnostics, and
recovery procedures are documented in
  [zebra-compat Node Operation](https://github.com/valargroup/zcashd/blob/feat/unity/doc/zebra-compat.md);
the Zebra-side sections of this page still apply.

## Quick start (externally managed)

Start Zebra's zcashd-compat RPC endpoint:

```console
zebrad start --zcashd-compat
```

On first start, Zebra:

1. runs Linux hardware and filesystem preflight checks (see
   [Hardware preflight](#hardware-preflight-linux));
2. starts the dedicated zcashd-compat RPC listener on `127.0.0.1:28232` with
   cookie authentication;
3. waits for an externally managed `zcashd -zebra-compat` process to connect.

The startup log banner shows the zcashd-compat RPC URL and cookie file.

To opt into supervision, set `manage_zcashd = true`. With the default
`zcashd_source = "path"`, Zebra requires `zcashd_path` to point at a local
binary. Set `zcashd_source = "managed"` to use Zebra's SHA256-pinned managed
download.

### Verify the integration

`getzebracompatinfo` is the primary status surface on the zcashd side:

```console
zcash-cli getzebracompatinfo
```

Confirm `zebra.identity_verified` is `true` and watch `readiness` move from
`degraded` (syncing) to `ready` once the local `zcashd` tip catches up to
Zebra's best tip. Expect `"p2p": false` and `"blocksource": "zebra"`.

Confirm `zcashd` has no peer-to-peer activity:

```console
zcash-cli getconnectioncount   # expect 0
zcash-cli getpeerinfo          # expect []
```

Confirm `zcashd` is not listening on the network P2P port (Zebra should be):

```console
ss -ltnp 'sport = :8233'    # mainnet
ss -ltnp 'sport = :18233'   # testnet
```

## What zcashd-compat mode does

When you start Zebra with `zebrad start --zcashd-compat`, Zebra:

- enables zcashd-compat mode (`[zcashd_compat].enabled = true`);
- ensures a dedicated zcashd-compat RPC listen address is configured (defaults to `127.0.0.1:28232`);
- uses dedicated cookie auth at `zcashd_compat.cookie_dir/zcashd_compat.cookie_file_name` by default;
- optionally serves the dedicated zcashd-compat RPC listener over TLS;
- raises the zcashd-compat RPC `max_response_body_size` to at least `128` MiB
  for large batched block responses;
- validates coordinated zcashd batch-size, zcashd response-budget, and Zebra
  response-body settings before startup;
- optionally spawns and supervises `zcashd -zebra-compat`.

If zcashd-compat supervision is enabled, Zebra starts `zcashd` with:

```text
-zebra-compat
-zebra-compat-url=<rpc_url>
[-zebra-compat-cookiefile=<zcashd_compat.cookie_dir>/<zcashd_compat.cookie_file_name> | -zebra-compat-no-auth=1]
[-zebra-compat-tls-ca-file=<zcashd_compat.tls_ca_file>]
-zebra-compat-zebra-rpc-max-response-body-bytes=<effective Zebra compat RPC limit>
-datadir=<zcashd_compat.zcashd_datadir or state.cache_dir/zcashd-compat-zcashd>
[-testnet | -regtest]
-p2p=0
-listen=0
-printtoconsole
[<zcashd_compat.zcashd_extra_args...>]
```

Zebra always passes `-p2p=0` and `-listen=0` before `zcashd_extra_args`. CLI
arguments are parsed before `zcash.conf` and are not overwritten by config-file
values, so legacy full-node configs with `listen=1` do not cause `zcashd` to
bind the network P2P port (8233 mainnet / 18233 testnet) that Zebra already
uses. `zcashd` also force-disables P2P boolean flags when `-zebra-compat` is
active, including later `zcashd_extra_args` such as `-p2p=1` or `-listen=1`.
Peer-selection options in `zcashd_extra_args` are rejected by `zcashd` startup
validation rather than silently taking effect.

Cookie auth remains the default in supervised mode. Zebra only passes
`-zebra-compat-no-auth=1` when `[zcashd_compat].enable_cookie_auth = false`,
which is accepted only for TLS-enabled zcashd-compat RPC listeners.

## Configuration

zcashd-compat mode adds a `[zcashd_compat]` section:

```toml
[zcashd_compat]
enabled = false
manage_zcashd = false
zcashd_source = "path"                               # "path" or "managed"; used only when manage_zcashd = true
zcashd_path = "/path/to/local/zcashd"               # required when supervised zcashd_source = "path"
zcashd_datadir = "/path/to/zcashd/datadir"          # optional
zcashd_extra_args = ["-debug=1"]                    # optional extra args
listen_addr = "127.0.0.1:28232"                     # optional, default set when zcashd-compat is enabled
cookie_dir = "/path/to/cookies"                     # optional, defaults to <cache_dir>
cookie_file_name = ".zcashd-compat.cookie"          # optional, defaults to ".zcashd-compat.cookie"
enable_cookie_auth = true                           # optional, defaults to true
tls_cert_file = "/path/to/zebra.crt"                # optional, enables HTTPS with tls_key_file
tls_key_file = "/path/to/zebra.key"                 # optional, required with tls_cert_file
tls_ca_file = "/path/to/internal-ca.pem"            # optional, passed to supervised zcashd
unsafe_allow_remote_http = false                    # optional, allows non-loopback listen_addr without TLS
startup_delay = "1s"
restart_backoff = "2s"                              # base exponential backoff
restart_backoff_max = "5m"                          # maximum retry delay
restart_reset_after = "1h"
shutdown_grace_period = "300s"
```

When overriding `zcashd_extra_args` via environment variables, pass a JSON array string:

```console
ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-conf=/path/to/zcash.conf","-debug=1"]'
```

`zebrad` always adds `-printtoconsole` automatically for supervised `zcashd`.

If `manage_zcashd = false`, Zebra still applies the zcashd-compat RPC
guardrails and startup validation, but does not spawn `zcashd`; see
[Sync batch size and response limits](#sync-batch-size-and-response-limits)
for the externally managed workflow.

### zcashd binary resolution

If `manage_zcashd = true`, Zebra resolves `zcashd` as follows:

1. If `zcashd_path` is set, Zebra uses that local executable directly.
2. `zcashd_source = "path"` requires `zcashd_path` to be set.
3. `zcashd_source = "managed"` uses Zebra's embedded release manifest
   to fetch a compatible `zcashd` archive, verify its SHA256, cache it, and run it.
   Managed downloads are currently available only on `x86_64` Linux.

Managed downloads are cached under:

```text
<state.cache_dir>/zcashd-compat/bin/<release_tag>/<target>/zcashd
```

If managed artifacts are unavailable for the local platform, including Linux
`aarch64`, set `zcashd_path` to a local binary instead.

Managed download failures (missing target artifact, hash mismatch, transport
failures) fail closed before Zebra supervises `zcashd`.

### zcashd datadir and `zcash.conf`

Supervised `zcashd` still requires a normal datadir and `zcash.conf`. When
`manage_zcashd = true`, Zebra creates the configured datadir if it is missing
and bootstraps a minimal config file only when the effective `zcash.conf` is
absent. Existing operator configs are never overwritten.

Before creating the datadir or bootstrap config, Zebra checks that the effective
datadir and `zcash.conf` location can be used by the current user. Existing
`zcash.conf` files must be readable; if the config is missing, the parent
directory must be writable so Zebra can create the bootstrap file.

Minimum first-start `zcash.conf`:

```conf
i-am-aware-zcashd-will-be-replaced-by-zebrad-and-zallet-in-2025=1
# zcashd-compat: P2P is disabled; chain data comes from zebrad RPC.
# Do not add bind=, connect=, addnode=, or listen=1 here.
```

`zcashd` refuses to start without this deprecation acknowledgement. If an
existing config has missing or legacy P2P settings, Zebra logs clear warnings;
some P2P flags are forced off, while peer configuration options can still make
`zcashd` reject startup validation.

### Migrating a legacy `zcash.conf` from full-node use

Operators often reuse an existing `zcash.conf`. In compat mode:

- `listen=1`, `p2p=1`, `dnsseed=1`, and `listenonion=1` in the file may remain
  on disk but are overridden at startup (supervisor CLI plus `zcashd`
  `-zebra-compat` preset). They do not need manual removal for those flags.
  The same boolean flags are force-disabled if repeated in `zcashd_extra_args`.
- Remove or avoid P2P peer options that fail startup validation: `bind=`,
  `whitebind=`, `connect=`, `addnode=`, `seednode=`, and similar.

You do not need to add `listen=0` to `zcashd_extra_args`; the supervisor
already passes it.

## Listeners, authentication, and TLS

### Listener overview

A zcashd-compat deployment involves several listeners with similar-sounding
settings. Do not disable the wrong one:

| Listener | Default / typical | Role in zcashd-compat |
|---|---|---|
| **Zebra network P2P** (`network.listen_addr`) | enabled | Zebra syncs blocks from the Zcash network. Keep enabled. |
| **Zebra compat RPC** (`zcashd_compat.listen_addr`) | `127.0.0.1:28232` | Backend channel for supervised `zcashd -zebra-compat`. Cookie-auth by default; HTTPS/no-cookie is opt-in. Separate from `[rpc]`. |
| **Zebra user RPC** (`[rpc].listen_addr`) | optional | Operator-facing Zebra JSON-RPC (for example `127.0.0.1:8232`). |
| **zcashd network P2P** (`-listen`, port 8233/18233) | forced off | Must stay off; Zebra owns P2P. |
| **zcashd wallet RPC** (`-rpcbind`, `-rpcport`) | operator choice | Unrelated to `-listen`; configure separately if needed. |

The dedicated zcashd-compat RPC listener uses cookie authentication by default,
independent of the operator-facing `[rpc]` listener. zcashd-compat uses a
separate listener and separate authentication/TLS settings so operators can
keep user-facing Zebra RPC and the zcashd backend channel isolated, even when
both listeners share the same process.

### TLS for the zcashd-compat listener

To serve the zcashd-compat listener over HTTPS, configure both certificate and
private-key files:

```toml
[zcashd_compat]
listen_addr = "127.0.0.1:28232"
tls_cert_file = "/path/to/zebra.crt"
tls_key_file = "/path/to/zebra.key"
tls_ca_file = "/path/to/internal-ca.pem"
```

Non-loopback `zcashd_compat.listen_addr` values require TLS. Loopback listeners
can use the default plain HTTP channel because credentials stay on the local
host. `zcashd_compat.unsafe_allow_remote_http = true` overrides the TLS
requirement for deployments where another boundary secures the listener, such
as a private container network or VPN; treat it like zcashd's
`-zebra-compat-allow-remote-http` escape hatch and prefer TLS. If Zebra and
supervised zcashd run in the same container, prefer keeping
`zcashd_compat.listen_addr` on container loopback instead.

The TLS certificate must include an IP Subject Alternative Name for the
`listen_addr` IP (for example `IP:127.0.0.1`): supervised zcashd connects to
the raw IP address and verifies the certificate against it, so a certificate
with only DNS names fails verification.

When `manage_zcashd = true`, Zebra uses an `https://` `-zebra-compat-url` and
passes `-zebra-compat-tls-ca-file=<tls_ca_file>` to supervised zcashd. The CA
file should contain the public CA certificate zcashd needs to verify Zebra's
server certificate. It is not Zebra's private key.

If TLS is enabled later on the same loopback supervised listener, zcashd keeps
the existing trusted boundary active even though the supervisor changes the URL
scheme from `http://` to `https://`. The boundary still must match the configured
host, port, path, network, and genesis. For remote endpoints, changing the URL
scheme is treated as a source change and requires the normal zcashd recovery
checks.

### Disabling cookie auth (TLS only)

Cookie auth can be disabled for the dedicated zcashd-compat listener only when
TLS is enabled:

```toml
[zcashd_compat]
listen_addr = "127.0.0.1:28232"
tls_cert_file = "/path/to/zebra.crt"
tls_key_file = "/path/to/zebra.key"
tls_ca_file = "/path/to/internal-ca.pem"
enable_cookie_auth = false
```

In this mode, supervised zcashd receives `-zebra-compat-no-auth=1` instead of a
cookie file. Use this only when another layer controls access to the HTTPS
endpoint, such as Cloudflare Access, mTLS, IP allowlists, or a private network.
Without cookie auth, any client that can reach the listener can call the exposed
RPC methods.

## Sync batch size and response limits

Three settings must agree when increasing zebra-compat sync depth:

- `-zebra-compat-sync-batch-size=<blocks>`: how many raw blocks zcashd asks
  Zebra for in one JSON-RPC batch. zcashd defaults to `30`.
- `-zebra-compat-sync-response-budget-mb=<MiB>`: zcashd's memory budget for one
  batched raw-block response. zcashd defaults to `128` MiB, which allows a
  memory-clamped maximum batch of `33` blocks.
- `[rpc].max_response_body_size`: Zebra's HTTP response-body limit. Zebra floors
  the zcashd-compat listener to at least `128` MiB, and jsonrpsee applies this
  limit to the whole JSON-RPC batch response.

Zebra fails startup if these settings are inconsistent in `zcashd_compat`
configuration. For example, if `zcashd_extra_args` requests
`-zebra-compat-sync-batch-size=80`, Zebra reports the missing or undersized
settings needed to make that batch valid.

For supervised zcashd, configure the zcashd-side batch and response budget in
`zcashd_extra_args`; Zebra passes its effective response limit to zcashd
automatically using
`-zebra-compat-zebra-rpc-max-response-body-bytes=<bytes>`. For example, an
80-block batch uses a round `320` MiB budget:

```toml
[rpc]
max_response_body_size = 335544320

[zcashd_compat]
zcashd_extra_args = [
  "-zebra-compat-sync-batch-size=80",
  "-zebra-compat-sync-response-budget-mb=320",
]
```

If `manage_zcashd = false`, Zebra still applies zcashd-compat RPC guardrails and
validates any zcashd batch/budget flags present in `zcashd_extra_args`, but it
does not start zcashd and cannot pass command-line flags to it. Keep
`[rpc].max_response_body_size` configured in Zebra, and pass the zcashd-side
values explicitly to the external process:

```console
zcashd -zebra-compat \
  -zebra-compat-sync-batch-size=80 \
  -zebra-compat-sync-response-budget-mb=320 \
  -zebra-compat-zebra-rpc-max-response-body-bytes=335544320 \
  -zebra-compat-url=http://127.0.0.1:8232 \
  -zebra-compat-cookiefile=/path/to/zebra/.cookie
```

See zcashd's `doc/zebra-compat.md` for the full batch-size and reorg-depth
arithmetic.

## Hardware preflight (Linux)

When zcashd-compat mode is enabled, Zebra runs Linux-only startup preflight checks
for CPU, effective RAM, mount-aware provisioned disk space, and filesystem
permissions.

If zcashd-compat mode is enabled on a non-Linux host, Zebra fails startup by
default because zcashd-compat is currently Linux-only. You can explicitly bypass
this guardrail with `--unsafe-low-specs`.

If hardware is below minimum requirements, or required filesystem paths are not
usable by the current user, Zebra fails closed by default. If hardware is below
recommended requirements but above minimums, Zebra logs explicit warnings and
continues.

Use `--unsafe-low-specs` to bypass minimum-check failures only when you
explicitly accept degraded or unstable operation. This also downgrades
filesystem permission failures to warnings, but it does not grant permissions:
later datadir bootstrap, config creation, or managed `zcashd` download steps can
still fail with the underlying operating-system error.

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

### Filesystem permission checks (fail closed by default)

Zebra validates the paths that zcashd-compat needs before it creates any missing
directories or config files. Permission problems are collected into the same
aggregated preflight error as CPU, RAM, and disk failures, so operators can fix
all reported paths at once.

Preflight validates:

- Zebra's state cache directory (`state.cache_dir`);
- the zcashd-compat RPC cookie directory (`zcashd_compat.cookie_dir`);
- the effective supervised `zcashd` datadir, including `-datadir=` overrides in
  `zcashd_extra_args`;
- the effective `zcash.conf` path, including `-conf=` overrides in
  `zcashd_extra_args`;
- explicit `zcashd_path` executability when `zcashd_source = "path"` or
  `zcashd_path` is set;
- the managed `zcashd` cache directory when `zcashd_source = "managed"` and the
  cached binary is missing, stale, or has mismatched provenance.

For paths that do not exist yet, Zebra checks the nearest existing ancestor and
reports whether the target cannot be created because that ancestor is not
writable by the current user. Existing `zcash.conf` files must be readable, and
an existing `zcashd` binary must be executable.

## Containers

The standard container image does not enable zcashd-compat or include `zcashd`
by default. Release builds publish a separate `zfnd/zebra-zcashd-compat` image,
currently for `linux/amd64` only, or you can build a local compat image with:

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

The `compat-docker-start` target uses the safer single-container pattern:

```console
ZEBRA_ZCASHD_COMPAT__LISTEN_ADDR=127.0.0.1:28232
```

This listener is only used by the supervised `zcashd` process inside the same
container, so it does not need to be published to the host and does not need
`zcashd_compat.unsafe_allow_remote_http`. The target publishes zcashd's own RPC
port on host loopback instead:

```console
-p 127.0.0.1:8232:8232
```

## Monitoring and process lifecycle

Use `zcash-cli getzebracompatinfo` as the primary monitoring surface for the
integration: it reports overall `readiness` (`ready` / `degraded` / `failed` /
`disabled`), sync state, retry and backoff counters, mempool-mirror and
transaction-forwarding health, and the active trusted boundary. The
[zcashd-side documentation](https://github.com/valargroup/zcashd/blob/feat/unity/doc/zebra-compat.md)
describes the full readiness surface and the matching recovery procedures.

When zcashd-compat supervision is enabled (`zcashd_compat.enabled = true` and
`zcashd_compat.manage_zcashd = true`):

- If `zcashd` exits unexpectedly, Zebra's zcashd-compat supervisor restarts it using
  exponential backoff based on `restart_backoff`, capped at
  `restart_backoff_max`. Retries continue indefinitely until Zebra shuts down.
- If a supervised `zcashd` child runs for at least `restart_reset_after` before
  exiting, Zebra resets the consecutive restart count before applying the next
  restart decision. This keeps old, recovered exits from permanently consuming
  the backoff ramp.
- If the zcashd-compat supervisor later exits or returns a runtime error (for
  example, spawn failures while running), Zebra logs a warning and keeps running
  without zcashd supervision.
- Zebra exposes zcashd-compat supervision state through metrics:
  `zcashd_compat.supervisor.active`, `zcashd_compat.supervisor.disabled`, and
  `zcashd_compat.supervisor.exhausted`.
- In the default unlimited-retry policy, `zcashd_compat.supervisor.exhausted`
  should remain `0`. Production monitoring should alert if
  `zcashd_compat.supervisor.active` becomes `0`, or if logs show repeated
  supervised `zcashd` restarts.
- Startup-time zcashd-compat config validation is unchanged. For example, if
  `zcashd_compat.manage_zcashd = true`, `zcashd_source = "path"`, and
  `zcashd_path` is unset or cannot be resolved, Zebra startup fails with an
  error.
- If `zebrad` is shut down normally, it asks the zcashd-compat supervisor to stop
  `zcashd` gracefully: SIGTERM first, then SIGKILL after
  `shutdown_grace_period` if needed. A forced kill can interrupt `zcashd` wallet
  or chainstate flushes, so size the grace period for the local data set: large
  mainnet nodes can need several minutes, while small test nodes can override it
  lower. The supervisor logs a warning with the child pid if the SIGKILL last
  resort fires.
- Zebra waits for the supervisor task itself for `shutdown_grace_period` plus a
  fixed 30-second margin, so its own shutdown timeout cannot cut the graceful
  SIGTERM sequence short. The SIGTERM → grace → SIGKILL path in the supervisor
  is the only way Zebra force-kills `zcashd`: dropping the child handle (for
  example on a `zebrad` panic or an aborted supervisor task) leaves `zcashd`
  running so it can finish flushing.
- Supervised `zcashd` runs in its own process group. Terminal signals aimed at
  `zebrad`'s process group (for example Ctrl-C in a wrapper script, or a
  group-wide kill) do not reach `zcashd` directly; the supervisor delivers
  SIGTERM to it during graceful shutdown instead.
- If `zebrad` is terminated ungracefully (for example `kill -9`), normal
  shutdown handlers do not run, so `zcashd` remains running until it is stopped
  externally. An orphaned `zcashd` keeps holding its datadir lock, so a
  restarted supervised `zebrad` retries spawning with backoff until the orphan
  exits or is stopped; this is visible in the supervisor logs and recovers on
  its own once the orphan is gone.
- Pair the supervisor settings with zcashd's `-zebra-compat-flush-interval`
  (default 300 seconds): it bounds how much chainstate an unclean `zcashd` death
  can lose, and therefore how much replay a restart needs. See the
  [zcashd-side documentation](https://github.com/valargroup/zcashd/blob/feat/unity/doc/zebra-compat.md)
  for the crash-recovery details.

## Quick regtest loop

1. Configure the network:

   ```toml
   [network]
   network = "Regtest"
   ```

2. Start Zebra in zcashd-compat mode:

   ```console
   zebrad start --zcashd-compat
   ```

3. Confirm the startup log banner shows the zcashd-compat RPC URL and cookie file.

4. Use `zcash-cli getzebracompatinfo` to verify Zebra identity and readiness.

5. Generate blocks via Zebra RPC (`generate`) and verify `zcashd` follows.

The zcashd-compat regtest suite also includes reorg regression and stress tests;
use `make compat-test-soak` for extended local churn runs.

Regtest interoperability can depend on matching assumptions between Zebra and
`zcashd` builds. If regtest semantics diverge, use testnet for initial
interoperability validation.
