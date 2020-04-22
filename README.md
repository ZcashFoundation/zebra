![Zebra logotype](https://www.zfnd.org/images/zebra-logotype.png)

---

[![](https://github.com/ZcashFoundation/zebra/workflows/CI/badge.svg?branch=main)](https://github.com/ZcashFoundation/zebra/actions?query=workflow%3ACI+branch%3Amain)
[![codecov](https://codecov.io/gh/ZcashFoundation/zebra/branch/main/graph/badge.svg)](https://codecov.io/gh/ZcashFoundation/zebra)
![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)

Hello! I am Zebra, an ongoing Rust implementation of a Zcash node.

Zebra is a work in progress.  It is developed as a collection of `zebra-*`
libraries implementing the different components of a Zcash node (networking,
chain structures, consensus rules, etc), and a `zebrad` binary which uses them.

Most of our work so far has gone into `zebra-network`, building a new
networking stack for Zcash, and `zebra-chain`, building foundational data
structures.

[Rendered docs from the `main` branch](https://doc.zebra.zfnd.org).

[Join us on Discord](https://discord.gg/na6QZNd).

## License

Zebra is distributed under the terms of both the MIT license
and the Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT).

## Metrics

Notes on local metrics collection:

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

Now the grafana dashboard is available at http://localhost:3030 ; the default password is admin/admin.
