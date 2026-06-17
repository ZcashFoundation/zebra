# Zakura P2P iroh Dependency

Plan: `/home/evan/src/valar/art/inbox/zakura_p2p/00.0_iroh_dependency.md`

## Decision

Zakura pins `iroh = "=0.92.0"` in the workspace with `default-features = false`.
The dependency is only wired into `zebra-network` scaffolding in this commit; no
Zakura protocol, endpoint service, handshake, relay, or discovery behavior is
enabled.

The latest stable iroh checked during implementation was `0.98.2`, but it could
not resolve with Zebra's current librustzcash dependency set. `iroh-base 0.98`
pulls the `sha2 0.11.0-rc` line, while the current Zcash stack pulls
`sha2 0.11.0-pre` through `bip32 0.6.0-pre.1`. `iroh 0.95.1` had the same
conflict through `ed25519-dalek 3.0.0-pre`. `iroh 0.92.0` is the newest iroh
line selected during review fixes that resolves with the current tree, uses the
patched `rustls-webpki 0.103.13` line, and still exposes the protocol router and
endpoint APIs needed by later Zakura plans.

`iroh 0.92.0` has `rust-version = "1.85"`, so this dependency does not raise
the workspace MSRV.

## Privacy Posture

Production defaults remain off. `zebra_network::zakura::direct_endpoint_builder`
constructs an iroh endpoint builder with:

- `RelayMode::Disabled`;
- `clear_discovery()`;
- a caller-provided durable `SecretKey`.

The iroh dependency is built with `default-features = false`, so iroh's optional
metrics feature is disabled and the optional `discovery-local-network` and
`discovery-pkarr-dht` features are not enabled. The local smoke test binds only
to `127.0.0.1:0`, reads back the endpoint `NodeAddr`, confirms there is at least
one direct address, and confirms both relay URL and discovery are absent.

With relays and external discovery disabled, v1 connectivity is limited to
directly reachable peers, local networks, forwarded ports, and later
legacy-upgrade address hints. Hard-NAT peers remain out of scope until a future
relay/discovery decision.

## API Names Confirmed

Against `iroh 0.92.0`, these names compile:

- `iroh::protocol::{Router, ProtocolHandler}`;
- `ProtocolHandler::accept(&self, iroh::endpoint::Connection) ->
  impl Future<Output = Result<(), iroh::protocol::AcceptError>> + Send`;
- `iroh::{Endpoint, RelayMode, SecretKey}`;
- `Endpoint::builder()`, `Endpoint::node_addr().initialized()`,
  `Endpoint::node_id()`, `Endpoint::discovery()`, and
  `Endpoint::add_node_addr()`;
- `iroh::NodeAddr`;
- `Connection::remote_node_id()`.

Later Zakura plans should target these names unless the iroh pin is changed.

## Dependency Reconciliation

The selected pin keeps the main TLS backend unified with Zebra's existing
reqwest stack:

- `rustls v0.23.40` is shared by reqwest and iroh;
- `ring v0.17.14` is shared by rustls and iroh;
- `rustls-webpki v0.103.13` is shared by rustls and iroh, with no older
  `rustls-webpki 0.102.x` copy left in `Cargo.lock`;
- iroh brings `iroh-quinn v0.14.0`, a renamed quinn stack, rather than the
  upstream `quinn` package currently pulled by reqwest HTTP/3 paths;
- smaller iroh subtree duplicates from the iroh-side network/interface helper
  crates are recorded narrowly in `deny.toml` for duplicate detection until the
  upstream crates converge.

`cargo deny check bans licenses sources` passes locally using cargo-deny 0.16.4.

## Reserved Identity Surface

`network.zakura_node_secret_key` is reserved as an optional explicit iroh
secret-key override. It is deserialized into a redacted config newtype so the
value does not appear in startup `Debug` logs or generated serialized config.
If it is unset, later Zakura endpoint construction will generate an ed25519 iroh
`SecretKey` on first use and persist it beside the network peer cache at:

```text
<cache_dir>/network/<network>.zakura-iroh-secret-key
```

This commit only reserves the config and storage path. It does not parse,
generate, persist, or use the key in production paths.

`CacheDir::zakura_node_secret_key_file_path` is the canonical helper for this
reserved storage path.
