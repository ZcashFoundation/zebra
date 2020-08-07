# Zebra Metrics

Zebra has support for Prometheus, configured using the [`MetricsSection`][metrics_section].

This requires supporting infrastructure to collect and visualize metrics, for example:

```
# create a storage volume for grafana (once)
sudo docker volume create grafana-storage
# create a storage volume for prometheus (once)
sudo docker volume create prometheus-storage

# run prometheus with the included config
sudo docker run --network host -v prometheus-storage:/prometheus -v /path/to/zebra/prometheus.yaml:/etc/prometheus/prometheus.yml  prom/prometheus

# run grafana
sudo docker run -d --network host -e GF_SERVER_HTTP_PORT=3030 -v grafana-storage:/var/lib/grafana grafana/grafana
```

Now the grafana dashboard is available at [http://localhost:3030](http://localhost:3030) ; the default username and password is `admin`/`admin`.

[metrics_section]: https://doc.zebra.zfnd.org/zebrad/config/struct.MetricsSection.html
