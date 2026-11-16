# Zebra checkpoints

A checkpoint is a trusted block height and hash compiled into Zebra. The
[checkpoint verifier] queues blocks until their hashes link two checkpoints,
then commits the chain as finalized state, avoiding most contextual checks
during initial synchronization. Review checkpoint updates as consensus-critical
data: they change which chain Zebra trusts.

## Checkpoint coverage and spacing

Mandatory checkpoints are a Zebra-specific mechanism, not a Zcash consensus
rule. Zebra cannot fully validate pre-Canopy blocks. Its
[mandatory checkpoint height] is the block immediately before Canopy:
1,046,399 on Mainnet and 1,028,499 on the default Testnet.

The [checkpoint_sync setting](../user/config.md#consensus) selects the full list
or its first checkpoint at or above the mandatory height. Blocks above the
selected checkpoint use full verification.

The generator uses these [spacing constants]:

| Limit | Value | Effect |
| --- | --- | --- |
| `MAX_CHECKPOINT_HEIGHT_GAP` | 400 blocks | Emit a checkpoint when the height gap reaches 400. |
| `MAX_CHECKPOINT_BYTE_COUNT` | 32 MiB (33,554,432 bytes) | Emit a checkpoint when cumulative serialized block size reaches this threshold. |
| `MAX_BLOCK_REORG_HEIGHT` | 1,000 blocks | Exclude the recent chain segment when generating checkpoints. |

The byte threshold is evaluated after adding a block, so an interval can
exceed 32 MiB. These limits bound queued work and memory, not protocol-valid
checkpoint placement.

The generator reads the source node's height once and considers only heights
strictly below that height minus 1,000. It always includes genesis when
starting a new list. Otherwise, it starts immediately after `--last-checkpoint`
and emits only new checkpoint lines. Each line has the form `height hash`.

## Automated updates

The [checkpoint update workflow] consumes artifacts from
[Integration Tests on GCP]. The generator jobs use synced Mainnet and Testnet
states. A successful integration run on `main` triggers the update workflow;
a manual dispatch selects the latest successful run with both artifacts:

- `generate-checkpoints-mainnet-checkpoints`
- `generate-checkpoints-testnet-checkpoints`

The workflow appends entries above each file's last height, validates the
result, and opens or updates the `chore/update-checkpoints` pull request for:

- `zebra-chain/src/parameters/checkpoint/main-checkpoints.txt`
- `zebra-chain/src/parameters/checkpoint/test-checkpoints.txt`

The workflow skips runs missing either artifact and unchanged files. The pull
request links the source integration run.

## Manual generation

Use a synchronized node with [JSON-RPC enabled](../user/rpc.md#enabling-the-server).
Verify its network before collecting hashes. From the repository root, build
the utility from the same source as the checkpoint lists:

```sh
cargo build --release --locked -p zebra-utils \
  --features zebra-checkpoints --bin zebra-checkpoints
./target/release/zebra-checkpoints --help
```

The binary requires `zebra-utils/zebra-checkpoints`; the
[same-named zebrad feature](../user/install.md#cargo-features) enables tests.

The utility accepts these options:

| Option | Default | Meaning |
| --- | --- | --- |
| `-b`, `--backend` | `zebrad` | Use `zebrad` for raw-block RPC responses, or `zcashd` for verbose responses containing height, hash, and size. |
| `-t`, `--transport` | `cli` | Use `zcash-cli` as a subprocess, or `direct` for HTTP JSON-RPC. |
| `-c`, `--cli` | `zcash-cli` | RPC client executable for the `cli` transport. |
| `-a`, `--addr` | Transport-dependent | RPC IP address and port. Direct mode defaults to `127.0.0.1:8232`; CLI mode otherwise uses the client's configuration. |
| `-l`, `--last-checkpoint` | None | Last existing checkpoint height; omit only when generating from genesis. |
| `--` followed by arguments | None | Pass arguments to `zcash-cli`, such as `-testnet` or authentication settings. |

Direct transport sends no authentication credentials. For the example below,
use a dedicated local node with `[rpc] listen_addr = "127.0.0.1:8232"` and
`[rpc] enable_cookie_auth = false`. Never disable authentication on a publicly
reachable endpoint. To retain [RPC authentication](../user/rpc.md#authentication),
use CLI transport with the node's credentials configured in `zcash-cli`.

Generate a Mainnet extension into a temporary output file rather than
appending directly to the tracked list:

```sh
checkpoints=zebra-chain/src/parameters/checkpoint/main-checkpoints.txt
last_checkpoint=$(tail -n 1 "$checkpoints" | cut -d ' ' -f 1)
./target/release/zebra-checkpoints \
  --backend zebrad --transport direct --addr 127.0.0.1:8232 \
  --last-checkpoint "$last_checkpoint" > new-main-checkpoints.txt
```

For Testnet, use `test-checkpoints.txt`, a Testnet node's RPC address, and a
separate output file. The utility has no network-selection flag: direct mode
uses the network of the node at `--addr`; CLI mode can pass `-- -testnet`.
The `zebrad` backend also works with `zcashd` raw-block responses.

Check that generation exited successfully before appending the output.
Generation can leave a partial output file after an RPC failure. An empty
successful output means that no new checkpoint met the spacing thresholds.

## Review and validation

1. Compare every added height and hash with the source run's artifact or the
   source node's block at that height. Check the network, source height, and
   settlement margin. Preserve existing entries during a routine extension.
2. Check that the first new checkpoint continues the existing list without
   duplicates or a gap above 400 blocks. For independent confirmation, compare
   the added hashes with another fully synchronized node on the same network.
3. Append the reviewed output to the corresponding tracked file. Remove the
   temporary output files after retaining the source evidence in the pull
   request.
4. Run the structural validator and checkpoint-list tests:

   ```sh
   .github/scripts/validate-checkpoints.sh zebra-chain/src/parameters/checkpoint/main-checkpoints.txt
   .github/scripts/validate-checkpoints.sh zebra-chain/src/parameters/checkpoint/test-checkpoints.txt
   cargo nextest run --locked -p zebra-chain checkpoint_list
   ```

The script checks line format, genesis height, increasing heights, duplicate
heights and hashes, and the 400-block maximum gap. Rust tests also check
mandatory coverage and the minimum gap derived from the byte threshold and
maximum block size. Neither establishes that a hash belongs to the intended
chain; review the source data.

[checkpoint verifier]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/checkpoint.rs
[mandatory checkpoint height]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/parameters/network.rs
[spacing constants]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/parameters/checkpoint/constants.rs
[checkpoint update workflow]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/checkpoint-update.yml
[Integration Tests on GCP]: https://github.com/ZcashFoundation/zebra/actions/workflows/zfnd-ci-integration-tests-gcp.yml
