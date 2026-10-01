# Dependency Trains

Zebra's Cargo dependencies are updated by two rolling pull requests, the _dependency trains_, instead of dependabot. A workflow (`.github/workflows/dependency-trains.yml`) runs `cargo update` on the first of every month at 06:00 UTC and regenerates both PRs in place. The design and its history are on [issue #9630](https://github.com/ZcashFoundation/zebra/issues/9630).

## The two trains

| Train | Branch | Moves | Merged by |
| --- | --- | --- | --- |
| Train A | `deps/train-a` | Everything except the consensus-sensitive crates | The conductor, monthly |
| Train B | `deps/train-b` | Only the consensus-sensitive crates: `zcash_*`, `orchard`, `halo2*`, `incrementalmerkletree`, `bridgetree`, `equihash`, `sapling-crypto`, `rocksdb`, `librocksdb-sys`, `secp256k1`, `zcash_script` | The ZODL-lane owner |

The consensus list lives in `.github/dependency-trains.toml`. Train B is reviewed like any other consensus change, never skim-merged.

Every train:

- only carries _semver-compatible_ bumps, because `cargo update` never crosses a breaking line. Breaking upgrades (majors, and 0.x minors) stay manual: the PR body lists every direct dependency "held behind a breaking line", which doubles as the standing majors dashboard.
- holds back any version published less than seven days ago (the _cooling-off_ filter), so the train only ships releases that survived a week of ecosystem scrutiny.
- writes one `zebrad` change fragment (`Updated dependencies via dependency train ...`), so the changelog gate stays green.
- is left alone by the cron once approved. A push would dismiss the approval, so approve a train only when you intend to merge it.

## The monthly routine

Three merges, about fifteen minutes when CI is green:

1. **Train A**: check the moved-crates table for anything surprising, check CI, approve and merge. If CI is red, pin back the offending crate (below), re-run the workflow from the Actions tab and merge the regenerated train.
2. **Train B**: hand off to the ZODL-lane owner, who reviews the consensus bumps and merges.
3. **The devops PR**: dependabot's `github-actions` group (label `devops`), which dependabot keeps refreshing weekly on its own. If one action's major breaks a workflow, add a dependabot `ignore` rule for that version and merge the rest.

The zebra-hub dashboard shows the age of each train; a train older than 45 days is a signal that the routine has stalled.

## Pinning back a broken crate

When the newest compatible release of a crate breaks the build, do not hold the whole train: pin that crate back so the rest ships.

1. Add the crate to `[pinned]` in `.github/dependency-trains.toml`, for example `nix = "0.30.1"`.
2. Open a PR with that change (it is a normal `ci:` PR) and file or link an issue for the upstream breakage.
3. The next train run applies the pin and lists it under "Pinned back" in the PR body. Remove the line once the crate is fixed upstream.

To run a train by hand, trigger the `Dependency Trains` workflow from the Actions tab and choose `a`, `b` or `both`.

## Previewing a train locally

The script that builds the trains runs anywhere with `cargo` and network access, and it changes nothing but `Cargo.lock`:

```sh
python3 .github/scripts/dependency-train.py a --dry-run --body-out /tmp/train-a.md
git checkout Cargo.lock
```

It takes about ten minutes, almost all of it the crates.io lookups (one request per second) behind the cooling-off filter and the "held behind a breaking line" table. `/tmp/train-a.md` is the exact PR body the workflow would post, including that table, so this is the quickest way to see which direct dependencies currently need a manual, breaking upgrade. Use `b` for the consensus train.

## Breaking upgrades

`cargo update` only moves a crate within the range declared in `Cargo.toml`: with `tower = "0.4"` the train can take 0.4.13 to 0.4.14, never to 0.5. Under cargo's rules a 0.x minor (0.4 to 0.5) is as breaking as a 1.x major. Crossing that line means raising the version floor in the root `Cargo.toml` and, almost always, fixing code against a changed API. No tool does that safely, so it stays human work.

The one-line model: _the trains move the lock, humans move the floors._

The train PR body lists every direct dependency whose newest stable release is behind a breaking line. That table is the backlog. It is long (44 crates at the time of writing) and nobody is expected to clear it; the aim is to drain it slowly and never let it grow silently.

How a breaking upgrade happens:

1. Pick one crate, or one family that must move together, from the table. Families are easy to spot: the `rand`/`rand_chacha`/`rand_core` trio, the `jsonrpsee` crates, the `opentelemetry` crates, `ff` with `group`.
2. Open an issue for it and put it on the release board. Aim for one or two per release; a breaking upgrade that misses a release just waits for the next.
3. Raise the floor in `[workspace.dependencies]` in the root `Cargo.toml` (three-component floors, matching the tested lock), fix the code, and open a normal PR. It carries a change fragment for every crate whose behaviour a user could notice.
4. Consensus-sensitive crates (`zcash_*`, `orchard`, `halo2*`, `sapling-crypto`, `zcash_script`) follow [Updating the ECC dependencies](ecc-updates.md); a breaking wave there is release engineering, not a dependency chore.

After the upgrade merges, the next train lists one row fewer.

## Rules that never change

- **45 days**: if a train has been open for 45 days, _any_ maintainer may merge it, pinning back whatever is broken. The process has no single point of failure.
- **Security fixes ship alone**: a RUSTSEC or GHSA fix is a standalone `cargo update -p <crate>` PR, merged immediately. Never queue a security fix behind a train.
- **Breaking upgrades are manual**: the trains never cross a breaking line. See [Breaking upgrades](#breaking-upgrades) below.
