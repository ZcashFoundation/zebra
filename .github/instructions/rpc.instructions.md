---
applyTo: "zebra-rpc/**"
---

# Safety review rules: RPC

Breaking these rules can split consensus, let peers or RPC callers exhaust the node, crash it, or corrupt its state. Flag a rule only where the diff introduces or extends the problem, give the file and line, and say what to change.

## Match zcashd RPC formats and responses exactly

Match zcashd's RPC parameter formats and defaults, and its response field names, presence, shape, JSON types, nulls, hex encoding, and hash byte order. Check against the zcashd source, or client source such as lightwalletd, before merging.

Look for: new or renamed `#[serde(rename)]` fields; `Option` fields that serialize as `null` where zcashd omits them, or the reverse; raw byte fields not hex-encoded; hashes serialized without the reversed display byte order; parameter defaults that differ from zcashd when omitted

## Surface missing state data as errors

When a zebra-state lookup or a required RPC field returns nothing, return an error or leave the value as `None`. Fail the whole request when any lookup it depends on fails.

Look for: `unwrap_or_default()` or `unwrap_or(0)` on state responses; `.unwrap_or(Height(0))` or a genesis hash fallback; missing required request fields filled with defaults; responses that succeed after some lookups failed

## Compute responses from current state and config

Derive tip-dependent and network-dependent response fields from the chain tip and the configured network, not from constants. Resolve file paths against the configured directory, and read shared inputs without mutating them.

Look for: hard-coded values in responses that should depend on the tip or `Network`; block size checks using the wrong Equihash solution size for the header variant; relative paths used without joining the configured directory; `&mut` access to a shared input that later code reads
