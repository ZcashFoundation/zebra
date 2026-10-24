# Zebra Observability Stack

Metrics, distributed tracing, and alerting for Zebra nodes.

## Quick Start

Export `GF_SECURITY_ADMIN_PASSWORD` with a strong, unique password in the shell
running Compose. An unset or empty value prevents startup.

```bash
# Start the full observability stack
docker compose -f docker/docker-compose.observability.yml up -d

# View logs
docker compose -f docker/docker-compose.observability.yml logs -f zebra
```

## Components

| Component | Host port | URL | Purpose |
|-----------|-----------|-----|---------|
| **Zebra** | 8233 | - | Zcash node with metrics and tracing |
| **Prometheus** | 9094 | <http://localhost:9094> | Metrics collection and storage |
| **Grafana** | 3000 | <http://localhost:3000> | Dashboards and visualization |
| **Jaeger** | 16686 | <http://localhost:16686> | Distributed tracing UI |
| **AlertManager** | 9093 | <http://localhost:9093> | Alert routing |

Zebra RPC (`8232`) and metrics (`9999`) are not published to the host. RPC
requires the cookie in Zebra's cache volume; Prometheus scrapes `zebra:9999`.
Prometheus's admin and lifecycle APIs are disabled. Restrict the published
monitoring ports to loopback or a private network.

Log in to Grafana as `admin` with the password supplied through
`GF_SECURITY_ADMIN_PASSWORD`. This initializes new Grafana volumes only;
change existing account passwords in Grafana.

## Architecture

```text
┌─────────────────────────────────────────────────────────────────────┐
│                         Zebra Node                                  │
│  ┌─────────────────┐              ┌─────────────────────────────┐  │
│  │ Metrics         │              │ Tracing (OpenTelemetry)     │  │
│  │ :9999/metrics   │              │ OTLP HTTP → Jaeger          │  │
│  └────────┬────────┘              └──────────────┬──────────────┘  │
└───────────│──────────────────────────────────────│──────────────────┘
            │                                      │
            ▼                                      ▼
┌───────────────────┐                  ┌───────────────────────────┐
│   Prometheus      │                  │        Jaeger             │
│   :9094           │                  │   :16686 (UI)             │
│                   │◄─────────────────│   :8889 (spanmetrics)     │
│   Scrapes metrics │  Span metrics    │   :4318 (OTLP HTTP)       │
└─────────┬─────────┘                  └───────────────────────────┘
          │                                        │
          ▼                                        │
┌───────────────────┐                              │
│     Grafana       │◄─────────────────────────────┘
│     :3000         │      Trace queries
│                   │
│  Dashboards for   │
│  metrics + traces │
└─────────┬─────────┘
          │
          ▼
┌───────────────────┐
│   AlertManager    │
│   :9093           │
│                   │
│  Routes alerts    │
└───────────────────┘
```

## What Each Component Provides

### Metrics (Prometheus + Grafana)

Quantitative data about Zebra's behavior over time:

- **Network health**: Peer connections, bandwidth, message rates
- **Sync progress**: Block height, checkpoint verification, chain tip
- **Performance**: Block/transaction verification times
- **Resources**: Memory, connections, queue depths

See [grafana/README.md](grafana/README.md) for dashboard details.

### Tracing (Jaeger)

Request-level visibility into Zebra's internal operations:

- **Distributed traces**: Follow a request through all components
- **Latency breakdown**: See where time is spent in each operation
- **Error analysis**: Identify failure points and error propagation
- **Service Performance Monitoring (SPM)**: RED metrics for RPC endpoints

See [jaeger/README.md](jaeger/README.md) for tracing details.

### Alerts (AlertManager)

Automated notifications for operational issues:

- Critical: Negative value pools (ZIP-209 violation)
- Warning: High RPC latency, sync stalls, peer connection issues

Configure alert destinations in [alertmanager/alertmanager.yml](alertmanager/alertmanager.yml).

## Configuration

### Environment Variables

Set these variables in the Zebra service's `environment` section:

| Variable | Default | Description |
|----------|---------|-------------|
| `ZEBRA_METRICS__ENDPOINT_ADDR` | - | Prometheus metrics endpoint (e.g., `0.0.0.0:9999`) |
| `ZEBRA_RPC__LISTEN_ADDR` | - | RPC endpoint for JSON-RPC requests |
| `ZEBRA_TRACING__OPENTELEMETRY_ENDPOINT` | - | Jaeger OTLP endpoint (e.g., `http://jaeger:4318`) |
| `ZEBRA_TRACING__OPENTELEMETRY_SERVICE_NAME` | `zebra` | Service name in traces |
| `ZEBRA_TRACING__OPENTELEMETRY_SAMPLE_PERCENT` | `100` | Sampling percentage (0-100)* |

`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`, and `OTEL_TRACES_SAMPLER_ARG`
are lower-precedence fallbacks for these tracing settings. They do not override
the `ZEBRA_TRACING__OPENTELEMETRY_*` values already set by this stack.

### Trace Sampling

During high-volume operations (initial sync), reduce sampling to avoid overwhelming the collector:

```yaml
# docker-compose.observability.yml
zebra:
  environment:
    - ZEBRA_TRACING__OPENTELEMETRY_SAMPLE_PERCENT=10  # 10% sampling during sync
```

| Scenario | Percentage | Notes |
|----------|------------|-------|
| Initial sync | 1-10 | High volume, sample sparingly |
| Steady state | 10-50 | Normal operation |
| Debugging | 100 | Full traces for investigation |

*Note: Zebra uses percentage (0-100) for consistency with other config options, unlike the standard OpenTelemetry ratio (0.0-1.0).

## Directory Structure

```text
observability/
├── README.md                    # This file
├── alertmanager/
│   └── alertmanager.yml         # Alert routing configuration
├── grafana/
│   ├── README.md                # Dashboard documentation
│   ├── dashboards/
│   │   ├── network_health.json  # Default home dashboard
│   │   └── *.json               # Other dashboards
│   └── provisioning/
│       ├── dashboards/
│       └── datasources/
├── jaeger/
│   ├── README.md                # Tracing documentation
│   └── config.yaml              # Jaeger v2 configuration
└── prometheus/
    ├── prometheus.yaml          # Scrape configuration
    └── rules/                   # Recording and alert rules
```

## Common Tasks

### View Zebra's current metrics

```bash
docker compose -f docker/docker-compose.observability.yml exec zebra \
  curl --fail --silent http://127.0.0.1:9999/metrics
```

### Query Prometheus directly

```bash
# Current block height
curl -s 'http://localhost:9094/api/v1/query?query=zcash_state_tip_height'
```

### Find slow operations in Jaeger

1. Open <http://localhost:16686>
2. Select service: `zebra`
3. Set "Min Duration" to filter slow traces
4. Click "Find Traces"

### Check spanmetrics for RPC performance

```bash
curl -s http://localhost:8889/metrics | grep rpc_request
```

## Troubleshooting

### No metrics in Grafana

1. [Retrieve Zebra's metrics inside its container](#view-zebras-current-metrics).
2. Check Prometheus targets: <http://localhost:9094/targets>
3. Verify job name matches dashboard queries (default: `zebra`)

### No traces in Jaeger

1. Verify OTLP endpoint is set: `ZEBRA_TRACING__OPENTELEMETRY_ENDPOINT=http://jaeger:4318`
2. Check Jaeger is healthy: <http://localhost:16686>
3. Check Jaeger logs: `docker compose logs jaeger`

### High memory usage in Jaeger

Reduce trace sampling during sync:

```yaml
environment:
  - ZEBRA_TRACING__OPENTELEMETRY_SAMPLE_PERCENT=10  # Sample only 10%
```

## Related Documentation

- [Grafana Dashboards](grafana/README.md) - Dashboard usage and configuration
- [Jaeger Tracing](jaeger/README.md) - Distributed tracing guide
- [Metrics Quick Start](../../docs/metrics-visualization-quickstart.md) - Adding new metrics
