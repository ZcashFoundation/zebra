# API reference

- [docs.rs](https://docs.rs/zebrad/latest/zebrad/#zebra-crates) documents the
  latest published release of each crate.
- The [internal documentation](https://zebra.zfnd.org/internal) documents
  `main`, including private items; every push to `main` regenerates it.
  Build it locally with:

  ```sh
  cargo doc --no-deps --workspace --all-features --document-private-items
  ```

- [JSON-RPC server](user/rpc.md) documents the RPC interface; a running node
  also describes its methods through the `rpc.discover` method (OpenRPC).
