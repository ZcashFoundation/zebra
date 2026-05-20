# Zebra Grafana Dashboards

Pre-built dashboards for monitoring Zebra nodes.

## Quick Start

```bash
# From repository root - starts Zebra + all observability tools
docker compose -f docker/docker-compose.observability.yml up -d
```

Access Grafana at <http://localhost:3000> (admin/admin - you'll be prompted to change on first login).

For full stack documentation, see the [Observability README](../README.md).

## Dashboards

| Dashboard | Description |
|-----------|-------------|
| `network_health.json` | Peer connections, bandwidth (default home) |
| `syncer.json` | Sync progress, block downloads |
| `mempool.json` | Transaction pool metrics |
| `peers.json` | Peer connection details |
| `block_verification.json` | Block verification stats |
| `checkpoint_verification.json` | Checkpoint sync progress |
| `transaction-verification.json` | Transaction verification |
| `network_messages.json` | P2P protocol messages |
| `errors.json` | Error tracking |

## Datasources

Grafana is provisioned with two datasources:

| Datasource | UID | Description |
|------------|-----|-------------|
| Prometheus | `zebra-prometheus` | Metrics storage and queries |
| Jaeger | `zebra-jaeger` | Distributed tracing |

Configuration: `provisioning/datasources/datasources.yml`

### Using Jaeger in Grafana

The Jaeger datasource allows you to:

- Search traces by service name
- View trace details and span timelines
- Correlate traces with metrics (via trace IDs)

To explore traces:

1. Go to **Explore** in Grafana
2. Select **Jaeger** datasource
3. Search for service `zebra`

Or access Jaeger UI directly at <http://localhost:16686> for full trace exploration.
See the [Jaeger README](../jaeger/README.md) for detailed tracing documentation.

## Dashboard Configuration

### Rate Window Requirements

Dashboards use `rate()` functions for per-second metrics. The rate window must
contain at least 2 data points to calculate a rate.

| Scrape Interval | Minimum Rate Window |
|-----------------|---------------------|
| 500ms           | 1s                  |
| 15s (default)   | 30s                 |
| 30s             | 1m                  |

Current dashboards use `[1m]` windows, compatible with the default 15s scrape interval.

If you modify `../prometheus/prometheus.yaml` scrape_interval, update dashboard queries accordingly.

### Job Label

The `$job` variable in dashboards is populated from Prometheus. The default job
name is `zebra` (configured in `../prometheus/prometheus.yaml`).

## Creating New Dashboards

### Option 1: Grafana UI Export (Recommended)

1. Create panel in Grafana UI
2. Click panel title → "Inspect" → "Panel JSON"
3. Add to dashboard file
4. Commit

### Option 2: Copy Existing Panel

1. Find similar panel in existing dashboard
2. Copy JSON, update metric names and titles
3. Test in Grafana

### Panel Template

```json
{
  "title": "Your Metric",
  "type": "timeseries",
  "targets": [
    {
      "expr": "rate(your_metric_total[1m])",
      "legendFormat": "{{label}}"
    }
  ],
  "fieldConfig": {
    "defaults": {
      "unit": "reqps"
    }
  }
}
```

## Validation

```bash
# Check JSON syntax
for f in dashboards/*.json; do jq . "$f" > /dev/null && echo "$f: OK"; done

# List all metrics used
jq -r '.panels[].targets[]?.expr' dashboards/*.json | sort -u
```
