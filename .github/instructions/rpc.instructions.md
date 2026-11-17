---
applyTo: "zebra-rpc/**"
excludeAgent: "cloud-agent"
---

# RPC review

- Check each changed method against its documented compatibility contract and the affected clients. For zcashd-compatible JSON-RPC methods, compare parameter defaults, field presence, nulls, JSON types, and hash display byte order. Preserve documented differences such as the field subsets returned by `getinfo` and `getdeprecationinfo`. For the Zebra indexer and lightwalletd gRPC services, use their protobuf and client contracts.
- Check that fields derived from the same chain tip come from one state snapshot. For example, flag an independent tip lookup when the transaction query already provides the tip associated with its confirmation count.
- Distinguish required state data from optional results. Flag a fabricated height or hash when a required lookup fails. Preserve omissions, nulls, and defaults where the method contract calls for them.
