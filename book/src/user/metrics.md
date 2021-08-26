# Zebra Metrics

Zebra has support for Prometheus, configured using the [`MetricsSection`][metrics_section].

This requires supporting infrastructure to collect and visualize metrics, for example:

1. Install Prometheus and Grafana via Docker

```
# create a storage volume for grafana (once)
sudo docker volume create grafana-storage
# create a storage volume for prometheus (once)
sudo docker volume create prometheus-storage

# run prometheus with the included config
sudo docker run --detach --network host --volume prometheus-storage:/prometheus --volume /path/to/zebra/prometheus.yaml:/etc/prometheus/prometheus.yml  prom/prometheus

# run grafana
sudo docker run --detach --network host --env GF_SERVER_HTTP_PORT=3030 --env GF_SERVER_HTTP_ADDR=localhost --volume grafana-storage:/var/lib/grafana grafana/grafana
```

Now the grafana dashboard is available at [http://localhost:3030](http://localhost:3030) ; the default username and password is `admin`/`admin`.
Prometheus scrapes Zebra on `localhost:9999`, and provides the results on `localhost:9090`.

2. Configure Grafana with a Prometheus HTTP Data Source, using Zebra's `metrics.endpoint_addr`.

In zebrad.toml:
```
[metrics]
endpoint_addr = "127.0.0.1:9999"
```

In the grafana dashboard:
1. Create a new Prometheus Data Source `Prometheus-Zebra`
2. Enter the HTTP URL: `127.0.0.1:9090`
3. Save the configuration

Now you can add the grafana dashboards from `zebra/grafana` (Create > Import >
Upload JSON File), or create your own.

[metrics_section]: https://doc.zebra.zfnd.org/zebrad/config/struct.MetricsSection.html
