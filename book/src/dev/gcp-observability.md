# GCP Observability Environment

This page documents how Zebra’s GCP observability works, based strictly on this repository’s code and configuration.

## Architecture

- Compute platform: Google Compute Engine using Container-Optimized OS.
- Deployment model: Managed Instance Groups (MIGs) per network, created from instance templates built from the Zebra Docker image.
- Container image: Built by CI/CD and referenced by GCE instance templates.
- Logging and monitoring: GCE metadata enables Google Cloud Logging (via fluent-bit) and Google Cloud Monitoring for VM/container metrics.
- Labels and tags: Instances are labeled to make log filtering easy (e.g., `app=zebrad`, `network=Mainnet|Testnet`).

The instance template is created with:

```startLine:318:endLine:336:.github/workflows/cd-deploy-nodes-gcp.yml
           gcloud compute instance-templates create-with-container zebrad-${{ needs.versioning.outputs.major_version || env.GITHUB_REF_SLUG_URL }}-${{ env.GITHUB_SHA_SHORT }}-${NETWORK} \
           --machine-type ${{ vars.GCP_SMALL_MACHINE }} \
           --boot-disk-size=10GB \
           --boot-disk-type=pd-standard \
           --image-project=cos-cloud \
           --image-family=cos-stable \
           --subnet=${{ vars.GCP_SUBNETWORK }} \
           ${IP_FLAG} \
           --create-disk="${DISK_PARAMS}" \
           --container-mount-disk=mount-path='/home/zebra/.cache/zebra',name=${DISK_NAME},mode=rw \
           --container-stdin \
           --container-tty \
           --container-image ${{ vars.GAR_BASE }}/zebrad@${{ needs.build.outputs.image_digest }} \
           --container-env "NETWORK=${{ matrix.network }},LOG_FILE=${{ vars.CD_LOG_FILE }},SENTRY_DSN=${{ vars.SENTRY_DSN }}" \
           --service-account ${{ vars.GCP_DEPLOYMENTS_SA }} \
           --scopes cloud-platform \
           --metadata google-logging-enabled=true,google-logging-use-fluentbit=true,google-monitoring-enabled=true \
           --labels=app=zebrad,environment=${{ github.event_name == 'workflow_dispatch' && 'qa' || 'staging' }},network=${NETWORK},github_ref=${{ env.GITHUB_REF_SLUG_URL }} \
           --tags zebrad
```

- Network selection is passed to the container via `NETWORK` and written into the generated `zebrad.toml` by the entrypoint:

```startLine:44:endLine:51:docker/entrypoint.sh
[network]
network = "${NETWORK:=Mainnet}"
listen_addr = "0.0.0.0"
cache_dir = "${ZEBRA_CACHE_DIR}"
```

- On releases, the workflow extracts the major version to segregate MIGs by major version:

```startLine:115:endLine:139:.github/workflows/cd-deploy-nodes-gcp.yml
# If a release was made we want to extract the first part of the semver from the tag_name
# ...
outputs:
  major_version: ${{ steps.set.outputs.major_version }}
```

See the full workflow for details: `book/src/dev/continuous-delivery.md` and `.github/workflows/cd-deploy-nodes-gcp.yml`.

## Deploying a monitoring `zebrad` instance

Deployments happen automatically:
- On pushes to `main` and on releases, MIGs are created/updated per network. See `book/src/dev/continuous-delivery.md`.

Manual deployments (workflow dispatch) accept a `network` input (`Mainnet` or `Testnet`). The container receives `NETWORK` accordingly. Logging and Monitoring are enabled via instance metadata as shown above.

Inside the container, the default config also enables a Prometheus metrics endpoint if the image was built with the `prometheus` feature. The entrypoint writes:

```startLine:68:endLine:72:docker/entrypoint.sh
[metrics]
endpoint_addr = "${METRICS_ENDPOINT_ADDR:=0.0.0.0}:${METRICS_ENDPOINT_PORT:=9999}"
```

Note: Metrics are available only if the image is built with the `prometheus` feature. Build features are configured in CI (`vars.RUST_PROD_FEATURES`).

## Accessing Grafana

For dashboards and local exploration:
- Use the existing instructions in `book/src/user/metrics.md` and `docker/docker-compose.grafana.yml` to run Prometheus and Grafana locally and import dashboards from `grafana/*.json`.
- Prometheus scrape example is in `prometheus.yaml` (scrapes `localhost:9999`).

Useful dashboards in-repo:
- `grafana/network_health.json`
- `grafana/block_verification.json`
- `grafana/peers.json`

## Accessing Google Cloud Logging

Logs from the GCE instances are forwarded to Cloud Logging (enabled by instance metadata). To view logs:
- Open Google Cloud Console → Logging → Logs Explorer.
- Filter by instance labels set in the template, for example:
  - `labels.app="zebrad"`
  - `labels.network="Mainnet"` or `"Testnet"`

Example advanced queries you can adapt:
- Show all `zebrad` logs for Mainnet MIG instances:
  - `resource.type="gce_instance" labels.app="zebrad" labels.network="Mainnet"`
- Search for startup lines indicating the selected network:
  - `resource.type="gce_instance" text:"network: Mainnet"`

Labels and metadata are added in the instance template as shown above.

## Example PromQL queries

The dashboards reference the following metrics; you can use similar PromQL queries:
- Latest verified height:
  - `zcash_chain_verified_block_height`
- Network throughput:
  - `rate(zcash_net_in_bytes_total[1m])`
  - `rate(zcash_net_out_bytes_total[1m])`
- Mempool size (examples vary across panels): see `grafana/mempool.json` and `zebrad` code metrics in `zebrad/src/components/mempool/*`.

See also the user metrics guide: `book/src/user/metrics.md`. 
