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

## Historical data availability

Requests for known historical block or transaction bodies that are no longer stored fail explicitly.
They are not treated as unknown objects, and block responses never contain only the remaining
transactions.

- JSON-RPC uses code `-32603` and `"Block not available (pruned data)"` for `getblock` verbosity
  0, 2, or 3, and for [field queries](user/rpc.md#getblock-field-queries) that request `txdetails`,
  `txprevouts`, `authdataroot`, or `hex`, including a pruned parent needed for verbosity 3 or
  `txprevouts`. For `getrawtransaction`, the message is `"Transaction not available (pruned data)"`.
  Unknown objects retain their existing errors.
- The indexer and native lightwalletd gRPC services use `FAILED_PRECONDITION` with the unavailable
  height. Unknown objects use `NOT_FOUND`; operational errors retain their existing statuses.
  A block-range or address-transaction stream attempts to send a terminal error if it encounters
  unavailable history, even after sending earlier results. A stalled consumer can instead see
  the stream close after the send timeout without receiving that error. Clients must check range
  completeness rather than treating earlier results or stream closure as proof of completion.
- Historical `getblock` verbosity 1 still returns the complete transaction-ID list, and field
  queries that request only index-backed fields, such as `tx`, still succeed. Headers,
  treestates, subtrees, address balances, address transaction IDs, and current UTXOs do not require
  historical transaction bodies. Treestate lookups report unknown blocks separately from header
  corruption or operational failures; those failures are not gRPC `NOT_FOUND` responses.
- The `getblockchaininfo` response reports `pruned` from actual body availability and includes
  `pruneheight` only when a historical prefix is missing. This is the start of the complete retained
  suffix, not a retention target; genesis is retained separately.

See the [zcashd API migration guide](user/zcashd-api-migration-guide.md#pruned-history) for JSON-RPC
compatibility details.
