# Zebra health endpoints

Zebra's HTTP/1.1 health listener provides liveness and readiness probes.
Liveness checks for recently responsive peers; readiness also checks sync
progress before a load balancer routes client traffic to the node.

Enable it with `[health] listen_addr`; no optional Cargo feature is needed:

```toml
[health]
listen_addr = "127.0.0.1:8080"
min_connected_peers = 1
ready_max_blocks_behind = 2
ready_max_tip_age = "5m"
enforce_on_test_networks = false
```

## Configuration

See the [configuration reference](config.md#health) for fields and defaults.

The field `[health] ready_max_tip_age` measures local progress, not the latest
block header's timestamp. The progress task samples the chain about every five
seconds; only a height above its previous maximum resets the timer.

The health server refreshes its peer count from the address book every five
seconds. Peers count only while marked as having responded within the network's
response recency window; failed or pending connections do not count. This is
not a count of open TCP sockets.

## Endpoints

All responses use `Content-Type: text/plain; charset=utf-8`.

### Liveness: `GET /healthy`

| HTTP status | Body | Condition |
| --- | --- | --- |
| `200 OK` | `ok` | Recently live peer count is at least `min_connected_peers`. |
| `503 Service Unavailable` | `insufficient peers` | Recently live peer count is below the threshold. |

This endpoint does not require a chain tip or a completed initial sync.
It applies the same peer check on every network, including Regtest. Setting
`min_connected_peers = 0` makes it succeed whenever the listener responds,
but also removes the peer requirement from readiness.

### Readiness: `GET /ready`

On Mainnet, or when `enforce_on_test_networks = true`, Zebra evaluates these
checks in order and returns the first failure:

| HTTP status | Body | Condition |
| --- | --- | --- |
| `503 Service Unavailable` | `insufficient peers` | Recently live peer count is below `min_connected_peers`. |
| `503 Service Unavailable` | `syncing` | The synchronizer's recent sync lengths do not indicate proximity to the tip. |
| `503 Service Unavailable` | `no tip` | The progress task has no estimated remaining block count. |
| `503 Service Unavailable` | `tip_age=<seconds>s` | Time since the last observed height increase exceeds `ready_max_tip_age`. |
| `503 Service Unavailable` | `lag=<blocks> blocks` | Estimated remaining blocks exceed `ready_max_blocks_behind`. |
| `200 OK` | `ok` | All checks pass. |

The synchronizer's near-tip signal uses the moving average of recent sync
lengths; it is not the same as the estimated block lag. Readiness requires
both checks. Negative lag estimates are clamped to zero by the progress
task. Equality with either configured maximum passes the corresponding
check.

On test networks, the default `enforce_on_test_networks = false` bypasses
**all** readiness checks: `/ready` returns `200 OK` even with no peers or
chain tip. Set it to `true` before using readiness to gate test-network client
traffic. Regtest's synchronizer always reports near-tip status, but enabling
enforcement still applies the peer, tip, age, and lag checks. In a Regtest
network that mines only on demand, the tip-age check can fail between blocks.

### Other requests

Requests with a method other than `GET`, including `HEAD`, return
`405 Method Not Allowed`. The body is `method not allowed`, except that
HTTP responses to `HEAD` have no body. Unknown paths requested with `GET`
return `404 Not Found` with `not found`.

## Kubernetes and load balancers

Bind `[health] listen_addr = "0.0.0.0:8080"` inside a pod when the kubelet
must reach the listener through the pod IP. Restrict that port with network
policies rather than publishing it as a public service.

Use `/ready` as the readiness probe. Keep the pod out of service during
initial sync instead of restarting it for lack of readiness. A TCP liveness
probe avoids restarting a functioning node during a peer outage:

```yaml
livenessProbe:
  tcpSocket:
    port: 8080
  initialDelaySeconds: 10
  periodSeconds: 10
readinessProbe:
  httpGet:
    path: /ready
    port: 8080
  initialDelaySeconds: 10
  periodSeconds: 5
```

Use `httpGet` with `path: /healthy` for liveness only when insufficient peers
should trigger a restart. Allow enough startup and failure time for peer
discovery and temporary network outages; restarting cannot repair an
unreachable external network.

Route load-balancer traffic only after `/ready` returns 200; remove the node
on 503. Probe RPC separately: neither health endpoint checks the JSON-RPC
listener or executes a client-facing method.

## Security

The health listener has no authentication or TLS. It returns coarse node
state, including lag or time since progress on a failed readiness check.
Bind to loopback or an internal interface, and restrict access with firewall
rules or network policies. Failure to bind `listen_addr` terminates startup.
