# Elasticsearch block export

The optional `elasticsearch` Cargo feature exports newly finalized blocks
as JSON documents to Elasticsearch. Kibana can query those documents through
an Elasticsearch data view. This is separate from [Prometheus metrics](metrics.md)
and does not replace Zebra's RocksDB chain state.

This **experimental** exporter has
[failure modes](#indexed-data-and-failure-behavior) that lose batches or interrupt
node operation. Use a separate node rather than making production depend on it.

## Compatibility and security

Zebra pins the Rust Elasticsearch client to `8.17.0-alpha.1`. That is the
client library version, not a verified server-version requirement. The
repository does not specify a tested Elasticsearch/Kibana server-version
matrix. Do not assume compatibility with an arbitrary current server release.
Install and configure the services using the
[Elasticsearch documentation](https://www.elastic.co/docs/deploy-manage/deploy/self-managed)
and [Kibana documentation](https://www.elastic.co/docs/deploy-manage/deploy/self-managed/install-kibana).

Zebra connects to one configured URL using HTTP Basic authentication. The
current transport **disables TLS certificate validation**, including when
using HTTPS. It has no configuration field for a certificate authority or
certificate pinning. Keep the Elasticsearch connection on a trusted,
isolated network or the same host; do not send credentials across an
untrusted network with this integration.

The password remains plaintext in TOML despite being redacted from debug output.
Store the configuration file where only the Zebra account can read it, or mount
it through the deployment's secret mechanism. The environment override
[`ZEBRA_STATE__ELASTICSEARCH_PASSWORD` is rejected](config.md#environment-variables).

## Build and configure Zebra

From a source checkout, install with the
[`elasticsearch` feature](install.md#cargo-features):

```sh
cargo install --locked --path zebrad --features elasticsearch
zebrad generate -o zebrad-elasticsearch.toml
```

Set the [`[state]` Elasticsearch fields](config.md#state) for an account
authorized to create and write the target index. A build with this feature
exports automatically during normal startup; there is no runtime enable switch.

For a Testnet experiment, edit the generated file's sections:

```toml
[network]
network = "Testnet"
listen_addr = "[::]:18233"

[state]
cache_dir = "./zebra-elasticsearch-state"
elasticsearch_url = "https://localhost:9200"
elasticsearch_username = "elastic"
```

Set `[state] elasticsearch_password` in the configuration file before starting
Zebra. Use a dedicated empty `[state] cache_dir` if the index must include
blocks from genesis. Enabling the feature on an existing state database
exports only blocks finalized afterwards; it does not replay stored blocks.
Do not delete an existing production state to populate an experimental index.

Start the node:

```sh
zebrad -c zebrad-elasticsearch.toml start
```

## Indexed data and failure behavior

The [exporter][export-source] runs after a block commits to RocksDB and serializes
its Rust representation, not the JSON-RPC response format. It excludes
non-finalized blocks.

Zebra constructs the index name as `zcash_` followed by the lowercase network
name: `zcash_mainnet` on Mainnet and `zcash_testnet` on the default Testnet.
Each document's `_id` is the decimal block height. Indexing that height again
replaces the existing document.

During historical sync, Zebra sends batches of 24 blocks through the
Elasticsearch bulk API. When the current block's header timestamp is less
than four hours behind the local clock, it sends the accumulated batch on
each finalized block instead. This batching heuristic is independent of the
node's readiness checks.

The export path has these limitations:

- A failed ping request logs `Elasticsearch is not available, skipping block
  indexing` and discards the queued batch. There is no retry queue.
- A failed bulk request, an unreadable response, or a response reporting bulk
  errors triggers a panic rather than a recoverable indexing error.
- Export runs synchronously in the finalized-block commit path. Elasticsearch
  delays therefore delay that commit path.
- A partially filled batch has no shutdown flush or persistent storage. A
  restart does not reconstruct it from blocks already in RocksDB.

An Elasticsearch index can therefore have gaps even when Zebra's own state
is complete. Monitor exporter errors and verify index completeness before
using it for analysis that requires every block.

## Query with Kibana

After Elasticsearch accepts the first batch, create a Kibana data view for
the generated index, such as `zcash_testnet`. The documents include the
block header's `header.time` field. Select it as the time field if the
Elasticsearch mapping exposes it as a date, and use a time range that includes
the historical blocks being indexed.

Zebra does not provision Kibana, data views, index mappings, or dashboards.

[export-source]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-state/src/service/finalized_state.rs
