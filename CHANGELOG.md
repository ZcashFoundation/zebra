# CHANGELOG

All notable changes to Zebra are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org).

## [Zebra 1.0.0-beta.10](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.10) - 2022-05-19

Zebra's latest beta finalizes the support for `lightwalletd` RPC methods and continues with the testing of each of these features. Also, this beta sets the NU5 mainnet activation height.

### Added

#### RPC

- `z_gettreestate` RPC (#3990)

##### Tests

- grpc test for `GetTaddressBalanceStream` and `GetAddressUtxosStream` (#4407)
- snapshot tests for RPC methods responses (#4352 #4401)

#### CI

- `lightwalletd_update_sync` test to CI (#4269)
- `lightwalletd_full_sync` test to CI (#4268)

### Changed

- Set NU5 mainnet activation height and current network protocol version (#4390)
- NU5 mainnet dependency upgrades (#4405)
- Use the latest lightwalletd version (#4398)

#### Rust Dependencies

- orchard, redjubjub, jubjub, group, bls12_381, bitvec, halo2, jubjub, primitive_types, 
  librustzcash, zcash_history, zcash_encoding, bellman, zcash_script, incrementalmerkletree (#4405)
- vergen from 7.0.0 to 7.1.0 (#4420)
- tokio-util from 0.7.1 to 0.7.2 (#4406)
- inferno from 0.11.2 to 0.11.3 (#4357)
- tokio from 1.18.1 to 1.18.2 (#4358)
- prost from 0.10.2 to 0.10.3 (#4348)
- bech32 from 0.8.1 to 0.9.0 (#4394)

#### CI Dependencies

- google-github-actions/auth from 0.7.1 to 0.7.3 (#4404 #4419)
- tj-actions/changed-files from 19 to 20 (#4403)
- w9jds/firebase-action from 2.0.0 to 2.1.0 (#4402)

#### Others

- Rename workflow files (#3941)
- Added block hash and height to syncer errors (#4287)
- Drop sentry dependencies when enable-sentry feature is disabled (#4372)
- Deprecate gcr.io as a registry and build faster (#4298)
- Clippy: Remove redundant bindings, allocations, and generics (#4353)

#### Documentation

- Added "old state directories aren't deleted" to known issues (#4365)
- Added support for Mermaid to render graphs (#4359)
- Fix some typos (#4397)

### Fixed

#### RPC

- Use the Sapling activation height in gRPC tests (#4424)

#### State

- Return non-finalized UTXOs and tx IDs in address queries (#4356)
- List cached state files before or after tests (#4409)

#### CI and testing fixes

- Updated Cargo.lock check job name in patch workflow (#4428)
- Put gRPC tests behind an optional feature flag to fix production build issues (#4369)
- Stop failing the send transaction test (#4416)
- Require cached lightwalletd state for the send transaction tests (#4303)
- Errors in Docker entrypoint (#4411)
- Only use cached state disks with the same state version (#4391)
- Output length in some tests (#4387)
- Wrong file being referenced by CI (#4364)
- Make test selection and logging consistent (#4375)
- Allow builds over 1 hour and tests without the sentry feature (#4370)


## [Zebra 1.0.0-beta.9](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.9) - 2022-05-06

Zebra's latest beta continues our work on `lightwalletd` RPC methods, and contains some internal CI improvements.

### Added

#### RPCs

- Add a script for comparing zcashd and zebrad RPC responses (#4219)
- Add Rust tests for lightwalletd sync from Zebra (#4177)
- Add integration test to send transactions using lightwalletd (#4068)
- RPC test with fully synced Zebra (#4157)
- Log unrecognized RPC requests (#3860)
- Implement the `get_address_tx_ids` RPC method query (#4119)
- Implement `getaddressbalance` RPC (#4138)
- Add a query function for transparent UTXOs (#4111)

#### CI

- Add `sending_transactions_using_lightwalletd` test to CI (#4267)
- Add a `zebrad tip-height` utility command (#4289)
- Add `fully_synced_rpc_test` test to CI (#4223)
- Add a reusable workflow for deployable integration tests (#4271)
- Add wallet grpc tests (#4253)
- Implement reusable workflows for image building (#4173)
- Implement `getaddressutxos` RPC method. (#4087)


### Changed

- Increase block validation timeouts (#4156)
- Decrease the peer handshake timeout to 3 seconds, to speed up the initial sync (#4212)
- Use link-time optimisation in release builds (#4184)
- Add an extra block retry, to speed up the initial sync (#4185)
- Update Zebra's block hash checkpoints (#4183)
- Document coinbase rules, refactor to ease understanding (#4056)
- Disconnect from testnet peers using the first NU5 testnet rules (#3976)

#### RPCs

- Simplify RPC types and add documentation (#4218)

#### Documentation

- Add transaction index diagram to RFC-0005 (#4330)

#### CI

- Skip tests when doing a manual full sync (#4333)
- Add cached state version to disk images (#4314)
- Check specific files for each job when linting (#4311)
- Use debian for faster mounting and bump readiness time (#4276)
- Use docker instead of Konlet for GCP deployments in CI (#4252)
- Create a full sync disk to add the cached state inside (#4266)
- Increase the Zcash parameter fetch timeout (#4148)

### Fixed

- Fix testnet syncer loop on large Orchard blocks (#4286)

#### RPCs

- Fix some RPC response formats to match `zcashd` (#4217)
- Make Zebra RPC compatible with the `zcash-cli` RPC client (#4215)
- Use a structure for parameters of getaddresstxids (#4264)

#### CI

- Only update cached states when needed (#4332)
- Run sync tests according to the right conditions (#4313)
- Stop actionlint from failing in main (#4317)
- Make the full sync tests cache state at `/zebrad-cache` (#4308)
- Avoid docker cache contamination and invalidation (#4254)
- Garbage collect instances no matter previous steps status (#4255)
- Do not delete instances from `main` branch on merge (#4206)
- Retry after docker log follow ssh failures (#4198)
- Share GitHub runner caches between branches (#4149)


## [Zebra 1.0.0-beta.8](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.8) - 2022-04-19

Zebra's latest beta completes our work on the NU5 consensus rules. It continues our work on `lightwalletd` RPC methods, and contains some internal CI improvements.

### Added

#### RPCs

- Implement a retry queue for transactions sent via RPCs (#4015)
- Partially implement the `getaddresstxids` RPC method (#4062)

#### State

- Add a transparent address balance index (#3963)
- Add a transparent address UTXO index (#3999)
- Add a transparent address transaction index (#4038)
- Add transparent address indexes to the non-finalized state (#4022)
- Add a query function for transparent address balances (#4097)

### CI

- Create cached state disk image after a successful full sync test (#3986)

### Changed

#### NU5

- Update to new zcash_script V5 API (#3799)
- Update network protocol versions for the second NU5 activation on testnet (#3799)
- Update the consensus branch ID and the second NU5 testnet activation height (#3799)
- Bump database version to trigger testnet rollback (#3799)

#### State

- Store transactions in a separate database index, to improve query speed (#3934)
- Store UTXOs by transaction location rather than transaction hash (#3978)
- Stop storing redundant transparent output fields in the database (#3992)
- Use LZ4 compression for RocksDB (#4027)
- Use Ribbon filters for database index lookups (#4040)
- Tune state database file compaction configuration (#4045)

#### CI

- Put the state version in cached state disk names (#4073)
- Improve mergify merge throughput (#4094, #4120)
- Run cached state rebuilds in main branch (#4107)
- Use GitHub Branch Protection checks instead of Mergify (#4103, #4105)
- Lint and standardize the actions structure (#3940)

#### Rust Dependencies

- Update shared Zcash dependencies for the second NU5 activation on testnet (#3799)
- Disable unused rocksdb compression features (#4082)
- Bump rlimit from 0.7.0 to 0.8.3 (#4051)

#### CI Dependencies

- Bump docker/metadata-action from 3.6.2 to 3.7.0 (#4049)
- Bump google-github-actions/auth from 0.6.0 to 0.7.0 (#4050)
- Bump tj-actions/changed-files from 18.6 to 18.7 (#4065)
- Bump reviewdog/action-actionlint from 1.21.0 to 1.23.0 (#4099, #4125)
- Bump actions/checkout from 3.0.0 to 3.0.1 (#4126)

### Fixed

#### CI

- Validate tests exit code after reading the container logs (#3968, #4069)
- Give enough time to zebra before reading logs (#4123)

#### Rust Clippy

- Ignore clippy drop warnings in tests (#4081)


## [Zebra 1.0.0-beta.7](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.7) - 2022-04-05

Zebra's latest beta fixes a `cargo install` build failure in the previous beta release. It also fixes a `lightwalletd` RPC bug, and improves test coverage.

### Changed

#### Database and State

- Update database design to put ordered list values in RocksDB keys (#3997)
- Make transparent address index database design more consistent (#4019)

#### CI

- Do not invalidate cache between PRs (#3996)

#### Dependency Updates

- Bump hyper from 0.14.17 to 0.14.18 (#3946)
- Bump indexmap from 1.8.0 to 1.8.1 (#4003)
- Bump semver from 1.0.6 to 1.0.7 (#3982)
- Bump serde-big-array from 0.3.2 to 0.4.1 (#4004)

##### Test Dependency Updates

- Bump insta from 1.13.0 to 1.14.0 (#3980)
- Bump tokio-util from 0.7.0 to 0.7.1 (#3981)

##### CI Dependency Updates

- Bump tj-actions/changed-files from 18.4 to 18.6 (#4002)

### Fixed

#### Build

- Fix a compilation error caused by a test-only method in production code (#4000)
- Add a job to ci.yml that does `cargo install --locked --path ./zebrad/ zebrad` (#3998)

#### RPC

- Tell `lightwalletd` to wait for missing blocks in the `getblock` RPC (#3977)

#### State

- Stop panicking when a state block commit fails (#4016)

#### Logging

- Log hashes as hex strings in block committment errors (#4021)

#### Tests

- Check for accidental database incompatibilities in cached state tests (#4020)

## [Zebra 1.0.0-beta.6](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.6) - 2022-03-28

Zebra's latest beta adds RPC server support, including some of the RPC calls needed to become a **lightwalletd** back end.
As part of the RPC changes, we made performance improvements to cached state access.

### Added

#### RPC

- RPC server support (#3589 #3863)
- `getinfo` RPC method (#3660)
- `sendrawtransaction` RPC method (#3685 #3706)
- `getbestblockhash` RPC method (#3754 #3864)
- `getblock` RPC method (#3707)
- `getrawmempool` RPC method (#3851)
- `getblockchaininfo` RPC method (#3891)
- `getrawtransaction`RPC method (#3908)

#### Tests

- Basic RPC server tests (#3641 #3726 #3879)
- Lightwalletd integration test (#3619 #3627 #3628 #3859 #3824 #3758 #3903)
- Full sync test (#3582)

#### Documentation

- Document RPC methods (#3868)
- Document consensus rules from 4.6 Action Descriptions (#3549)

#### CI

- Add lightwaletd integration test build and CI pipeline (#3657 #3700 #3705)

#### Others

- Added `TransactionsByMinedId` to mempool (#3907)
- Added code owners and automatic review assigment to the repository (#3677 #3708 #3718)
- Validate ZIP-212 grace period blocks using checkpoints (#3889)
- Store Sapling and Orchard note commitment trees in finalized and non-finalized state (#3818)
- Get addresses from transparent outputs (#3802)
- Explain the different ways .txt files are usedin the CI (#3743)

### Changed

The Zebra team made a huge refactor to the database storage and the state to serve RPC calls efficiently. The refactor is accompanied with extensive low level tests in the form of snapshots.

#### Database and State

- Tests: #3691 #3759 #3630
- Database: #3717 #3741 #3874 #3578 #3579 #3590 #3607 #3617
- State: #3778 #3810 #3846 #3847 #3870 #3865 #3866 #3811 #3826

#### CI

- Cleanup GCP instances on a single PR (#3766)
- Use OCI Image Format Specification for labels (#3728)
- Use improved OIDC for gcloud authentication (#3885)
- Use gcloud to search for cached disk state (#3775)

#### Dependency updates

- Remove an outdated dependabot ignore rule (#3719)
- Manually upgraded some dependencies (#3625)
- Replace unmantained multiset with mset (#3595)
- Update sha2 from 0.9.8 to 0.9.9 (#3585)
- Update semver from 1.0.5 to 1.0.6 (#3610)
- Update secp256k1 from 0.21.2 to 0.21.3 (#3632)
- Update insta from 1.12.0 to 1.13.0 (#3762)
- Update inferno from 0.10.12 to 0.11.1 (#3748 #3919)
- Update regex from 1.5.4 to 1.5.5 (#3797)
- Update google-github-actions/setup-gcloud from 0.5.1 to 0.6.0 (#3814)
- Update docker/login-action from 1.12.0 to 1.14.1 (#3570 #3761)
- Update actions/cache from 2 to 3 (#3918)
- Update tj-actions/changed-files from 14.4 to 18.4 (#3667 #3917 #3796 #3895 #3876)
- Update docker/build-push-action from 2.9.0 to 2.10.0 (#3878)
- Update once_cell from 1.9.0 to 1.10.0 (#3747)
- Update actions/checkout from 2.4.0 to 3.0.0 (#3806)
- Update owo-colors from 3.2.0 to 3.3.0 (#3920)
- Update vergen from 6.0.2 to 7.0.0 (#3837)

#### Documentation

- Simplify the database design using prefix iterators (#3916)
- Link to Conventional Commits specification in CONTRIBUTING file (#3858)
- Update database design for read-only state service (#3843)
- Explain optional zebra-network/tor dependencies (#3765)
- Simplify zebra-checkpoints summary (#3612)

#### Tests

- Turn on full backtraces and disable frame filtering (#3763)
- Decouple full sync from other tests (#3735)
- Split zebrad acceptance tests into sub-modules (#3901)
- Improve zebrad test API (#3899 #3892)

#### Others

- Move mempool request and response types to a new `zebra-node-services` crate (#3648)
- Enable `checkpoint_sync` by default (#3777)
- Update Zebra's hard-coded blockchain checkpoint lists  (#3606)
- Clippy lints: warn on manual printing to stdout or stderr (#3767)

### Removed

- Temporally removed Windows support from CI (#3819)

### Fixed

- Make FromHex consistent with ToHex for tx/block hashes (#3893)
- Prevent synchronizer loop when very close to tip (#3854)
- Use RwLock for note commitment tree root caches (#3809)

#### Tests

- Use the correct stop condition for the cached state tests (#3688)
- Stop excessive logging which causes test hangs (#3755)
- Use all available checkpoints in the full sync test (#3613)
- Use `TEST_FAKE_ACTIVATION_HEIGHTS` at runtime and fix tests (#3749)
- Check for zebrad test output in the correct order (#3643)

#### CI

- Do not use GHA cache for images (#3794)
- Run coverage collection when pushing to main (#3561)
- Check for duplicate dependencies with optional features off (#3592)
- Remove coverage from mergify because nightly builds fail (#3886)
- Only run the full sync test on mergify queue PRs (#3773)
- Fix syntax in some yml workflows (#3957)
- Update CI job path triggers (#3692)
- Change pd-extreme to pd-ssd to avoid quotas (#3823)
- Use a specific shortening length for SHAs (#3929)
- Path format for cached state rebuild (#3873)
- Re-enable manual dispatch for test full sync (#3812)
- Use correct conditional in job to deploy Mainnet nodes (#3750)
- Missing job key and timeout bump for build (#3744)

### Security

- Forbid non-ascii identifiers (#3615)


## [Zebra 1.0.0-beta.5](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.5) - 2022-02-18

Zebra's latest beta brings better networking, documents more consensus rules and
improves the CI pipelines.

### Added

- Estimate network chain tip height based on local node time and current best tip (#3492)
- Add extra integer lints, and partially fix some code (#3409)
- Prepare for changes in ZIP-244 (#3415, #3446)
- Support large block heights (#3401)
- Check coinbase data is at least 2 bytes long (#3542)
- Ignore non-verack and non-version messages in handshake (#3522)
- Allow forcing zebrad to use color output (#3547)

#### Tests

- Add a test for peerset broadcast panic (#3470)
- Add PeerSet readiness and request future cancel-safety tests (#3252)
- Add full chain synchronization acceptance tests (#3543)
- Add chain tip estimate test: log chain progress while Zebra is syncing (#3495)

#### Networking

- Avoid repeated requests to peers after partial responses or errors (#3505)
- Send notfound when Zebra doesn't have a block or transaction (#3466)
- Route peer requests based on missing inventory (#3465)
- Create an API for a missing inventory registry, but don't register any missing inventory yet (#3255)

#### Documentation

- Document consensus rules from 7.3 Spend Description Encoding and Consensus (#3575)
- Document second part of consensus rules from 7.6 Block Header Encoding and Consensus (#3566)
- Document consensus rules from 3.9 Nullifier Sets (#3521)
- Update README goals and performance troubleshooting (#3525)
- Document consensus rules from 4.5 Output Descriptions (#3462)
- Document shielded pools consensus rules from 7.1.2 Transaction Consensus Rules (#3486)
- Document Transaction encodings: sprout fields (#3499)
- Document Transaction encodings: transparent fields (#3498)
- Document Transaction encodings: orchard fields (#3507)
- Document Transaction encodings: sapling fields (#3501)
- Document Transaction encodings: header fields (#3491)
- Document consensus rules from 4.4 Spend Descriptions (#3460)
- Document consensus rules from 4.3 JoinSplit Descriptions (#3452)
- Document Transaction consensus rules: Size rules (#3461)
- Document Transaction consensus rules: Coinbase rules (#3464)
- Document Transaction consensus rules: Header rules (#3456)

### Changed

- Reduce log level of components (#3418, #3437)
- Change Type To Force Consensus Rule Validation (#3544)
- Split The Database Module (#3568)
- Dockerize Tests And Run Sync In Detached Mode (#3459)
- Improve Docker And Gcloud Usage Without Cloud Build (#3431)
- Make better use of variables, secrets and versions (#3393)

### Removed

- Remove founders reward code (#3430)

### Fixed

- Use the new `increase_nofile_limit` function from `rlimit` 0.7.0 (#3539)
- Generate Well-Formed Finalsaplingroot In Arbitrary Implementation (#3573)
- Rename some lightwalletd database types (#3567)

#### Networking

- Allow more inbound than outbound connections (#3527)
- Only send responded updates on handshake/ping/pong (#3463)
- Increase state concurrency and syncer lookahead (#3455)
- Add a send timeout to outbound peer messages (#3417)

#### Tests

- Make Full Sync Test More Accurate (#3555)
- Create Disk From Image Before Mounting (#3550)
- Simplify Resource Conflict Test To Avoid Ci Failures (#3537)
- Make Full Sync Test More Efficient (#3562)
- Evaluate "if" conditions correctly and use last disk SHA (#3556)

#### CI

- Improve test requirements and merge conditions for Mergify (#3580)
- Make The Purpose Of Each Sync Test Clearer (#3574)
- Delete A Redundant "Test All" Job (#3552)
- Allow Branches With Dots In The Name (#3557)
- Allow Unprivileged Runs Of Clippy (#3558)
- New Lints In Nightly Rust (#3541)
- Typo In Paths Filtering Keyword (#3516)
- Do Not Wait For Deprecated Cloud Build (#3509)
- Restrict Merges With Unresolved Threads (#3453)
- Put PRs With No Priority Label In The Low Priority Queue (#3454)
- Temporarily allow forked repos to run PR workflows (#3503)

## [Zebra 1.0.0-beta.4](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.4) - 2022-01-26

Zebra's latest beta improves the networking code and fixes some bugs. A couple of fixed bugs had
caused Zebra to hang in some situations. Some improvements to the documentation were also included.
All Rust crates have now been updated to use Rust 2021 Edition.

### Added

- Add a copy-state zebrad command, which copies blocks between two state services (#3175)

#### Networking

- Add isolated Tor connection API, but don't enable it by default (#3303)
- Add a test for message broadcast to the right number of peers (#3284)

### Changed

- Update to use Rust edition 2021 (#3332)

#### Networking

- Cache incoming unsolicited address messages, and use them as responses (#3294)
- Cleanup internal network request handler, fix unused request logging (#3295)

#### Documentation

- Document the consensus rules for Spend Transfers, Output Transfers, and their Descriptions (Section 3.6 of the Zcash protocol specification) (#3338)
- Document the structure of the zebra-network crate (#3317)
- Document the consensus rules for Note Commitment Trees (Section 3.8 of the Zcash Protocol Specification) (#3319)
- Document chain value balances consensus rules with new format (#3286)
- Document part of the block header consensus rules (#3296)

### Fixed

#### Consensus

- Fix interstitial sprout anchors check (#3283)
- Check jubjub key correctness independent of redjubjub / jubjub (#3154)

#### Networking

- Fix some bugs related to isolated connections (#3302)
- Ignore unexpected block responses to fix error cascade when synchronizing blocks, improving synchronization speed (#3374)
- Cancel heartbeats that are waiting for a peer, rather than hanging Zebra (#3325)
- Stop ignoring some peers when updating the address book (#3292)
- Fix some address crawler timing issues (#3293)
- Retry Zcash sprout and sapling parameters download (#3306)
- Keep track of background peer tasks (#3253)

#### Chain Synchronization

- Fix deadlock in chain tip watch channel, that sometimes caused chain synchronization to hang (#3378)
- Fix syncer download order and add sync tests (#3168)

#### Tests

- Fix a type resolution error in the tests (#3304)

### Security

- Stop RocksDB and Tokio from calling unexpected code when zebrad exits (#3392)

## [Zebra 1.0.0-beta.3](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.3) - 2021-12-21

Zebra's latest beta works towards enforcing all consensus rules by validating JoinSplit Groth16 proofs
used by Sprout transactions. We have also added security and network improvements, and have also
added some metrics to help diagnose networking issues.

### Added

#### Consensus

- Validate JoinSplit proofs (#3128, #3180)

#### Networking

- Add and use `debug_skip_parameter_preload` config option in tests (#3197)
- Disconnect from outdated peers on network upgrade (#3108)

#### Metrics

- Add diagnostics for peer set hangs (#3203)
- Add debug-level Zebra network message tracing (#3170)

### Fixed

- Stop ignoring some connection errors that could make the peer set hang (#3200)
- Spawn initial handshakes in separate tasks, Credit: Equilibrium (#3189)
- Fix coinbase height deserialization to reject non-minimal encodings (#3129)
- Stop doing thousands of time checks each time we connect to a peer  (#3106)

### Security

- Stop ignoring panics in inbound handshakes (#3192)
- When there are no new peers, stop crawler using CPU and writing logs  (#3177)
- Limit address book size to limit memory usage (#3162)
- Drop blocks that are a long way ahead of the tip, or behind the finalized tip (#3167)


## [Zebra 1.0.0-beta.2](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.2) - 2021-12-03

Zebra's latest beta continues implementing zero-knowledge proof and note commitment tree validation. In this release, we have finished implementing transaction header, transaction amount, and Zebra-specific NU5 validation. (NU5 mainnet validation is waiting on an `orchard` crate update, and some consensus parameter updates.)

We also fix a number of security issues that could pose a local denial of service risk, or make it easier for an attacker to make a node follow a false chain.

As of this release, Zebra will automatically download and cache the Sprout and Sapling Groth16 circuit parameters. The cache uses around 1 GB of disk space. These cached parameters are shared across all Zebra and `zcashd` instances run by the same user.

### Added

#### Network Upgrade 5

- Validate orchard anchors (#3084)

#### Groth16 Circuit Parameters

- Automatically download and cache Zcash Sapling and Sprout parameters (#3057, #3085)
- Stop linking the Sapling parameters into the `zebrad` and Zebra test executables (#3057)

#### Proof & Anchor Verification

- Use prepared verifying key for non-batch Sapling Groth16 verification (#3092)
- Validate sapling anchors⚓ (#3084)
- Add Sprout anchors to `zebra-state` (#3100)

#### Transaction Amount & Header Validation

- Validate miner transaction fees (#3067, #3093)
- Validate transaction lock times (#3060)
- Validate transaction expiry height (#3082, #3103)

#### Dashboards

- Add transaction-verification.json Grafana dashboard (#3122)

### Fixed

- Shut down channels and tasks on PeerSet Drop (#3078)
- Re-order Zebra startup, so slow services are launched last (#3091)
- Fix slow Zebra startup times, to reduce CI failures (#3104)
- Speed up CI, and split unrelated and conflicting CI jobs (#3077)

### Security

- Stop closing connections on unexpected messages, Credit: Equilibrium (#3120, #3131)
- Stop routing inventory requests by peer address (#3090)

## [Zebra 1.0.0-beta.1](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.1) - 2021-11-19

Zebra's latest beta implements a number of consensus rules which will be needed for Zebra to fully validate all of the Zcash network consensus rules, including those which will activate with NU5.

With this release we are also fixing a number of security issues that could pose a DDoS risk or otherwise negatively impact other nodes on the network.

Finally, this release includes an upgrade to the latest version of tokio (1.14.0).

### Added

- Check Shielded Input and Output Limits (#3069, #3076)
- Sprout note commitment trees (#3051)
- Check per-block limits on transparent signature operations (#3049)
- Calculate Block Subsidy and Funding Streams (#3017, #3040)
- Check for duplicate crate dependencies in CI (#2986)
- Add unused seed peers to the Address Book (#2974, #3019)

#### Network Upgrade 5

- Verify Halo2 proofs as part of V5 transaction verification (#2645, #3039)
- ZIP-155: Parse `addrv2` in Zebra (#3008, #3014, #3020, #3021, #3022, #3032)
- ZIP 212: validate Sapling and Orchard output of coinbase transactions (#3029)
- Validate Orchard flags in v5 (#3035)

#### Documentation

- Mempool Documentation (#2978)

### Changed

- Upgrade cryptographic library dependencies (#3059)
- Upgrade to Tokio 1.14.0 (#2933, #2994, #3062)

#### Documentation

- README Updates (#2996, #3006)

### Fixed

- Stop downloading unnecessary blocks in Zebra acceptance tests (#3072)
- Implement graceful shutdown for the peer set (#3071)
- Check for panics in the address book updater task (#3064)
- Remove unused connection errors (#3054)
- Fix listener address conflicts in network tests (#3031)

### Security

- Security: Avoid reconnecting to peers that are likely unreachable (#3030)
- Security: Limit number of addresses sent to peers to avoid address book pollution (#3007)

## [Zebra 1.0.0-beta.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-beta.0) - 2021-10-29

This is the first beta release of Zebra. Today the mempool work is fully finished and compatible with [ZIP-401](https://zips.z.cash/zip-0401) and several security issues in the network stack are fixed. In addition to that we improved our documentation specially in the `zebrad` crate while we increased our test coverage. Finally, we get started with the task of upgrading Tokio to version 1.

### Added

#### Mempool

- ZIP-401: weighted random mempool eviction (#2889, #2932)
- Reject a mempool transaction if it has internal spend conflicts (#2843)
- Limit transaction size in the mempool (#2917)

#### Cleanup and performance

- Remove unused mempool errors (#2941)
- Simplify calling add_initial_peers (#2945)
- Disable the new clippy::question_mark lint (#2946)
- Downgrade startup logs to debug to speed up CI (#2938)
- Speed up alternative state and zebrad tests in CI (#2929)

#### Tests

- Restore and update mempool tests (#2966)
- Test multiple chain resets (#2897)

### Security

- Track the number of active inbound and outbound peer connections so we can implement limits (#2912)
- Limit the number of initial peers (#2913)
- Rate-limit initial seed peer connections (#2943)
- Limit the number of inbound peer connections (#2961)
- Rate limit inbound connections (#2928)
- Limit the number of outbound peer connections (#2944)
- Reduce outgoing peers demand (#2969)

### Documentation

- Improve main `README` documentation and other book sections (#2967, #2894)
- Expand documentation for the mempool::crawler module (#2968)
- Improve mempool documentation (#2942, #2963, #2964, #2965)
- Improve documentation and types in the PeerSet (#2925)
- Update the documentation for value pools (#2919)
- Document why `CheckForVerifiedTransactions` is required for correctness (#2955)

#### Metrics

- Add user agent metrics (#2957)

### Changed

Part of the Tokio version 1 upgrade:

- Manually pin Sleep futures (#2914)
- Refactor handshake rate limiting to not store a Sleep type (#2915)
- Use single thread Tokio runtime for tests (#2916)

## [Zebra 1.0.0-alpha.19](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.19) - 2021-10-19

Zebra's latest alpha updates dependencies, improves metrics, gossips verified blocks and transactions, and continues the mempool-related implementation.

### Added

- Ignore `AlreadyInChain` error in the syncer (#2890)
- Give more information to the user in the wrong port init warning (#2853)
- Send looked up UTXOs to the transaction verifier (#2849)
- Return the transaction fee from the transaction verifier (#2876)
- Gossip recently verified block hashes to peers (#2729)
- Compute the serialized transaction size of unmined transactions (#2824)

#### Mempool

- Add a queue checker task, to make sure mempool transactions propagate (#2888)
- Store the transaction fee in the mempool storage (#2885)
- Add a debug config that enables the mempool (#2862)
- Pass the mempool config to the mempool (#2861)
- Add expired transactions to the mempool rejected list (#2852)
- Send AdvertiseTransactionIds to peers (#2823)
- Add a mempool config section (#2845)
- Add transactions that failed verification to the mempool rejected list (#2821)
- Un-reject mempool transactions if the rejection depends on the current tip
  (#2844)
- Split storage errors by match type: TXID or WTXID (#2833)
- Remove transactions in newly committed blocks from the mempool (#2827)

#### Documentation

- Improve the template for release versioning (#2906)
- Improve the documentation for the mempool transaction downloader (#2879)
- Add the documentation for the mempool storage and gossip modules (#2884)

#### Network Upgrade 5

- Set the minimum testnet network protocol version to NU5 (#2851)

#### Testing and CI

- Add some additional checks to the acceptance mempool test (#2880)

#### Metrics

- Refactor and add some new mempool metrics (#2878)
- Improve logging for initial peer connections (#2896)
- Always zero the mempool metrics when the mempool is disabled (#2875)
- Add a basic mempool storage Grafana dashboard (#2866)
- Add zcash.mempool.size.transactions and zcash.mempool.size.bytes metrics
  (#2860)
- Make some grafana labels shorter for graph readability (#2850)
- Make block metrics more accurate (#2835)

### Changed

#### Mempool

- Avoid broadcasting mempool rejected or expired transactions to peers (#2858)
- Encapsulate some mempool functions with the Mempool type (#2872)
- Remove duplicate IDs in mempool requests and responses (#2887)
- Refactor mempool spend conflict checks to increase performance (#2826)
- Rename mempool storage methods by match type (#2841)
- Remove unused mempool errors (#2831)

### Fixed

- Fix synchronization delay issue (#2921)
- Fix test failures by flushing output streams before exiting Zebra (#2911, #2923)
- Increase Zebra's restart acceptance test timeout (#2910)
- Avoid spurious acceptance test failures by decreasing the peer crawler timeout (#2905)
- Cancel pending download tasks when the mempool is disabled (#2886)
- Stop allowing some newly mined transactions into the mempool (#2874)
- Stop panicking when pruning unused queued blocks (#2842)

### Security

- Upgrade to ed25519-zebra 3.0.0 (#2864)
- Stop ignoring the mempool conflicting transaction reject list size limit (#2855)


## [Zebra 1.0.0-alpha.18](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.18) - 2021-10-05

Zebra's latest alpha updates dependencies, consensus parameters, and Orchard for NU5 testnet activation.
It continues our work on the mempool, including some mempool features that are used during network upgrade activation.

### Added

#### Mempool

- Send crawled transaction IDs to the mempool downloader (#2801)
- Cancel mempool download tasks when a network upgrade activates (#2816)
- Send mined transaction IDs to the mempool download/verify task for cancellation (#2786)
- Remove expired transactions from the mempool (#2774)
- Cancel download and verify tasks when the mempool is deactivated (#2764, #2754)
- Reject mempool transactions with conflicting spends (#2765)
- Clear mempool at a network upgrade (#2773, #2785)

#### Network Upgrade 5

- Update Zebra's advertised network protocol version to the latest NU5 testnet version (#2803)

#### Testing and CI

- Add tests to ensure mempool is working correctly (#2769, #2770, #2815)
- Create and use a helper MockService type to help with writing tests (#2810, #2748, #2790)
- Update Zebra tests to use the NU5 testnet activation height (#2802)
- Regenerate NU5 test cases with the latest network upgrade parameters (#2802)

#### Metrics

- Add Zebra metrics and a Grafana dashboard for connected peers and their network protocol versions (#2804, #2811)

### Changed

- Stop sending empty network messages to peers (#2791)
- Correctly validate transactions which never expire (#2782)

#### Network Upgrade 5

- Update `zcash_script` dependency to support V5 transactions (#2825)
- Set the NU5 testnet activation network upgrade parameters (#2802)
- Update shared Zcash Rust NU5 dependencies  (#2739)
- Update Zebra to use modified APIs in shared Zcash Rust NU5 dependencies (#2739)

### Fixed

- Stop panicking when using sync and async methods on the same `ChainTipChange` (#2800)
- Fix an incorrect assertion when the block locator is at the tip (#2789)
- Fix a missing NULL pointer check in `zebra_script`'s FFI (#2802)

### Security

#### Network Upgrade 5

- Update Zebra's orchard commitment calculations based on the latest orchard circuit (#2807)

## [Zebra 1.0.0-alpha.17](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.17) - 2021-09-14

Zebra's latest alpha continues work on the mempool.

### Added

- Monitor changes to the chain tip, decide if synchronization reached tip (#2695,
  #2685, #2686, #2715, #2721, #2722)
- Only enable the mempool crawler after synchronization reaches the chain tip (#2667)
- Reply to requests for transactions IDs in the mempool (#2720)
- Reply to requests for transactions in the mempool, given their IDs (#2725)
- Download and verify gossiped transactions (#2679, #2727, #2718, #2741)
- Internal additions and improvements to the mempool (#2742, #2747, #2749)

#### Documentation

- Document consensus rules for version group IDs (#2719)
- Specify Zebra Client will only support Unified Addresses (#2706)

### Fixed

-  Stop calculating transaction hashes twice in the checkpoint verifier (#2696)

### Security

- Replace older duplicate queued checkpoint blocks with the latest block's data (#2697)


## [Zebra 1.0.0-alpha.16](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.16) - 2021-08-27

Zebra's latest alpha finishes most of the state work needed for NU5 testnet activation and starts the mempool work.

### Added

#### Network Upgrade 5

- Finished ZIP-209: Prohibit Negative Shielded Chain Value Pool Balances (#2599, #2649, #2656, #2648, #2644)
- ZIP-221 and ZIP-244: Commitment validation in non-finalized state and checkpoint verifier (#2609, #2633)
- Orchard binding_verification_key support (#2441)

#### Mempool

- Mempool component, storage and errors (#2615, #2651)
- Create a transaction crawler for the mempool (#2646, #2672)
- Create and use types for unmined transactions and their IDs (#2634, #2666)
- ZIP-239: Implement mempool transaction v5 data structures and network messages (#2618, #2638)
- Return a transaction verifier from `zebra_consensus::init` (#2665)
- Provide recent syncer response lengths as a watch channel (#2602)
- Derive Copy and Clone for zebra-consensus errors (#2664)
- ChainTip trait creation and usage (#2677, #2676)

#### Metrics

- Add a network message to the grafana dashboard (#2673)

##### Documentation

- Refactor Zebra metrics docs (#2678, #2687)

### Fixed

- Stop converting `Message::Inv(TxId+)` into `Request::TransactionsById` (#2660)
- Fixed test.yml (#2635)
- Fixed a clippy lint (#2642)

## [Zebra 1.0.0-alpha.15](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.15) - 2021-08-16

Zebra's latest alpha contains the last of the changes to zebra chain state ahead of NU5 testnet activation and before concentrating on mempool work. The remaining NU5 validation will be completed prior to NU5 mainnet activation.

### Added

- Reject connections from outdated peers after network upgrade activation (#2519)

#### Network Upgrade 5

- ZIP-209: Prohibit Negative Shielded Chain Value Pool Balances Partial Implementation (#2546, #2554, #2561, #2569, #2576, #2577, #2578, #2596)
- ZIP-221: FlyClient - Consensus-Layer Changes Partial Implementation (#2531, #2553, #2583)
- ZIP-244: Implementation of authorizing data commitment (auth_digest) (#2547)
- Check remaining transaction value is non-negative (#2566)

### Changed

- Cache note commitment tree roots to speed up chain synchronisation  (#2584)
- Update state cache version to v9 (#2598)
- Refactor HistoryTree type into NonEmptyHistoryTree and HistoryTree types (#2582)

#### Testing and CI

- Reduce number of nullifier property test cases (#2574)
- Ensure `regenerate-stateful-test-disks.yml` syncs to the latest network upgrade (#2601)
- Increase coverage for generated chains and proptests (#2540, #2567)
- Remove unreliable tests for generated chains (#2548)
- Add test-only methods for modifying orchard shielded data and joinsplits (#2580)
- Generate test chains with valid chain value pools (#2597)
- Increase timeout of cached database creation (#2581)
- Use fixed genesis coinbase data in generated genesis blocks (#2568)
- Generate valid sapling shielded data for property tests (#2579)
- Optimize build to regenerate the test state faster (#2552)

### Fixed

- Fix the storage of anchors in the state (#2563)
- Make `zebra-state` compile successfully by itself (#2611)

#### Documentation

- Fix Transparent Value sign and definition (#2555)

## [Zebra 1.0.0-alpha.14](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.14) - 2021-07-29

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5.

### Added

- Reject UTXO double spends (#2511)
- Add a ValueBalance type (#2505)
- Implement Sum for Amount (#2500)
- Add an OrderedUtxo type for transparent spend validation (#2502)
- Pass the finalized state to chain contextual validation (#2503)
- Calculate incremental note commitment trees (#2407)
- Reject duplicate Sapling and Orchard nullifiers (#2497)
- Add `proptest-impl` feature to `zebra-state` crate to help simplify tests(#2529)
- Track anchors and note commitment trees (#2458)
- Validate spends of transparent coinbase outputs (#2525)
- Calculate the remaining value in the transparent transaction value pool (#2486)

#### Documentation

- Add value pools design to book summary (#2520)

#### Testing

- Test consensus-critical Amount deserialization (#2487)
- Update to use new GitHub action names in Google Cloud workflows (#2533)
- Add test intialization helper function for tests (#2539)

### Changed

- Decrease the number of randomised test cases in zebra-state (#2521, #2513)
- Make nullifier tests consistent with UTXO tests (#2513)

#### Testing

 - Disable Rust beta tests in CI, due to a Rust bug (#2542)

### Fixed

- Clarify a variable name and check for overflow in new_ordered_outputs (#2510)
- Update Orchard keys, hashes, and note commitments to match Zcash test vectors (#2445)

## [Zebra 1.0.0-alpha.13](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.13) - 2021-07-15

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5. New validation
rules were implemented for transactions, preventing double spends in the Sprout pool, checking
Sapling spends in V5 transactions, and some initial work to validate parts of Orchard transactions.

### Added

- Reject duplicate sprout nullifiers in the state (#2477)
- Add methods for getting block nullifiers (#2465)
- Verify orchard spend auth (#2442)
- Parse and ignore MSG_WTX inventory type in network messages (part of ZIP-239) (#2446)
- Add ZIP-244 signature hash support (#2165)
- Add HistoryTree struct (#2396)
- Add ZIP-0244 TxId Digest support (#2129)
- Validate V5 transactions with Sapling shielded data (#2437)

#### Documentation

- Document some consensus-critical finalized state behaviour (#2476)
- Value pools design (#2430)

### Changed

- Move Utxo type to zebra-chain (#2481)
- Combine near-duplicate Utxo creation functions (#2467)

#### Documentation

- Update state RFC for incremental trees, value pools, and RocksDB (#2456)
- Modify UTXO and state designs for transparent coinbase output checks (#2413)

### Fixed

- When a parent block is rejected, also reject its children (#2479)
- Restore the previous non-finalized chain if a block is invalid (#2478)
- Stop ignoring sapling binding signature errors (#2472)
- Always compute sighash with librustzcash (#2469)
- Fix bug in sighash calculation for coinbase transactions (#2459)
- Increase coverage of cached state tests: full block and transaction validation, and non-finalized
  state validation (#2463)

### Security

## [Zebra 1.0.0-alpha.12](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.12) - 2021-07-02

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5. It also includes documentation updates and security fixes. In particular, Zebra no longer gossips unreachable addresses to other nodes and users should update and restart any running Zebra nodes.

### Added

- Check if a previous version of Zebra followed a different consensus branch (#2366)
- API for a minimum protocol version during initial block download (#2395)
- Add a verification height to mempool transaction verification requests (#2400)
- Improved error messages by adding database path method to the Finalized State (#2349)
- Add property test strategies for V5 transactions (#2347)
- Re-use a shared Tokio runtime for some Zebra tests (#2397)

#### Documentation

- Add CHANGELOG.md file to the zebra git repo (#2346)
- Client RFC updates (#2367, #2341)
- Explain how Zebra validates shielded coinbase outputs like other shielded outputs (#2382)
- Document required request timeouts due to data dependencies (#2337)
- Update known issues and add inbound network ports to the README (#2373)
- Document shared to per-spend anchor conversion (#2363)
- Document note commitment trees storage (#2259)

#### Network Upgrade 5

- Orchard note commitment tree test vectors (#2384)
- Enable V5 transaction test vectors in the groth16 tests (#2383)
- Validate transparent inputs and outputs in V5 transactions (#2302)
- Batch math & variable-time multiscalar multiplication for RedPallas (#2288)
- Implement asynchronous verifier service for RedPallas (#2318)
- ZIP-211: Validate Disabling Addition of New Value to the Sprout Value Pool (#2399)

### Changed

- Various transaction verifier refactors (#2371, #2432)
- Remove unicode in Zebra's user agent (#2376)
- Update multiple crates to ensure bitvec 0.22.3 is being used (#2351)
- Move transaction consensus branch ID check function to zebra-chain (#2354)
- Replace primitives_types with uint (#2350)
- Refactor to return errors from state update methods to prepare for contextual validation (#2417)
- Refactor of the validation of Sapling shielded data (#2419)

#### Network Upgrade 5

- Update spend and output checks for new Orchard consensus rules (#2398)

### Fixed

- Failed tests in the cached state CI workflow are no longer ignored (#2403)
- Stop skipping the cached sync tests in CI (#2402)
- Fix intermittent errors in the groth16 verifier tests (#2412)
- Skip IPv6 tests when ZEBRA_SKIP_IPV6_TESTS environmental variable is set (#2405)
- Stop failing after the mandatory Canopy checkpoint due to incorrect coinbase script verification (#2404)
- Improved docs and panic messages for zebra_test::command (#2406)
- Gossip dynamic local listener ports to peers (#2277)
- Stop allowing JoinSplits for Halo (#2428)
- Fix failing legacy chain tests (#2427)

### Security

- Zebra no longer gossips unreachable addresses to other nodes (#2392)
  - User Action Required: Update and restart any running Zebra nodes
- Avoid duplicate peer connections (#2276)
- Send local listener address to peers (#2276)
- Limit reconnection rate to individual peers (#2275)

## [Zebra 1.0.0-alpha.11](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.11) - 2021-06-18

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5. It also includes some security fixes.

### Added

- Add and use a function for the mandatory checkpoint for a given Network (#2314)

#### Network Upgrade 5

- ZIP-221: integrate MMR tree from librustcash (without Orchard) (#2227)

### Changed

- Replace usage of atomics with tokio::sync::watch in tests for CandidateSet (#2272)

#### Network Upgrade 5

- Use latest librustzcash version in zcash_history (#2332, #2345)

#### Testing and Diagnostics

- Refactor restart_stop_at_height test to make it more flexible (#2315)
- Use Swatinem/rust-cache@v1 (#2291)
- Replace bespoke source-based coverage config with cargo-llvm-cov (#2286)
- Remove outdated pinned nightly in coverage workflow (#2264)

### Fixed

#### Network Upgrade 5

- Stop panicking on invalid orchard nullifiers (#2267)
- Reject V5 transactions before NU5 activation (#2285)

#### Testing

- Make acceptance test zebrad output matching more robust (#2252)

### Security

- Stop gossiping failure and attempt times as last seen times (#2273)
- Return an error rather than panicking on invalid reserved orchard::Flags bits (#2284)
- Only apply the outbound connection rate-limit to actual connections (#2278)
- Rate limit initial genesis block download retries, Credit: Equilibrium (#2255)

## [Zebra 1.0.0-alpha.10](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.10) - 2021-06-09

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5. It also includes some security fixes.

### Added

#### Network Upgrade 5

- Store Orchard nullifiers into the state (#2185)

#### Testing and Bug Reports

- Generate test chains that pass basic chain consistency tests (#2221)
- Make arbitrary block chains pass some genesis checks (#2208)
- Test Eq/PartialEq for Orchard keys (#2187, #2228)
- Further test new transaction consensus rules (#2246)
- Create workflow to regenerate cached state disks for tests (#2247)
- Add final sapling root test vectors (#2243)

### Changed

- Make sure the mandatory checkpoint includes the Canopy activation block (#2235)
- Move the check in transaction::check::sapling_balances_match to V4 deserialization (#2234)

#### Network Upgrade 5

- Implement more Transaction checks for Transaction Version 5 and Orchard (#2229, #2236)

#### Testing and Diagnostics

- Make debugging easier on proptests with large vectors (#2232, #2222)
- Update test job to use cached state version 5 (#2253)
- Add the database format to the panic metadata (#2249)

#### Developer Workflows

- Update the GitHub and RFC templates based on retrospectives (#2242)

### Fixed

- Get redpallas tweak proptests working again (#2219)

#### Testing

- Adjust the benchmark sample size so all benchmarks finish successfully (#2237)
- Fix scriptCode serialization and sighash test vectors (#2198)
- Allow multi-digit Zebra alpha versions in the zebrad acceptance tests (#2250)

### Security

- Don't trust future gossiped last seen times (#2178)
- Stop panicking when serializing out-of-range times, Credit: Equilibrium (#2203, #2210)
- Rate limit GetAddr messages to any peer, Credit: Equilibrium (#2254)
- Prevent bursts of reconnection attempts (#2251)

## [Zebra 1.0.0-alpha.9](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.9) - 2021-05-26

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5, and includes several security fixes.

### Added
- Added a new `zcash_serialize_bytes` utility function (#2150)
- Added new Arbitrary impls for a number of types in zebra-chain and zebra-network (#2179)
- Zebra support for leap seconds (#2195)

#### Network Upgrade 5
- Zebra can now serialize and deserialize orchard shielded data (#2116)
- We now have some Action methods for orchard shielded data (#2199)

#### Testing and Bug Reports
- Added extra instrumentation for initialize and handshakes (#2122)

### Changed
- Collect and send more accurate peer addresses (#2123)
- Enable cargo env vars when there is no .git during a build, fix tag lookup, add build profile, add modified flag (#2065)

#### Testing
- Stop generating V1-V3 transactions for non-finalized state proptests (#2159)
- Added some logging to troubleshoot failing tests for redpallas signature (#2169)

### Fixed

- Fix clippy::cmp_owned for (sapling, orchard)::keys with ConstantTimeEq (#2184)

#### Documentation
- Fixed some typos and links in the documentation(#2157, #2174, #2180)

### Security
- Reject compact sizes greater than the protocol message limit (#2155)
- Handle small numbers of initial peers better (#2154)
  - This security issue was reported by Equilibrium
- Stop panicking on out-of-range version timestamps (#2148)
  - This security issue was reported by Equilibrium
- Stop gossiping temporary inbound remote addresses to peers (#2120)
  - If Zebra was configured with a valid (not unspecified) listener address, it would gossip the ephemeral ports of inbound connections to its peers. This fix stops Zebra sending these useless addresses to its mainnet and testnet peers.
- Avoid silently corrupting invalid times during serialization (#2149)
- Do version checks first, then send a verack response (#2121)
- Remove checkout credentials from GitHub actions (#2158)
- Make CandidateSet timeout and initial fanout more reliable (#2172)
- Remove CandidateSet state and add last seen time limit to validate_addrs (#2177)

## [Zebra 1.0.0-alpha.8](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.8) - 2021-05-12

Zebra's latest alpha continues our work on NU5, including Orchard and Transaction V5.

### Added

#### Network Upgrade 5

- Continue implementation of Transaction V5 (#2070, #2075, #2100)
- Implementation of data structures for Orchard support in Zebra (#1885)
- Implementation of redpallas in Zebra (#2099)

#### Testing and Bug Reports

- Enable more Transaction v5 tests (#2063)

#### Documentation

- Document how Zebra does cross-crate proptests (#2069, #2071)
- Explain how to derive arbitrary impls in the dev docs (#2081)
- Async in Zebra RFC (#1965, #2111)
- Fixes and updates to Zebra Book TOC and process (#2124, #2126)
- Improvements to release process (#2138)
- Explicitly allow unencrypted disclosures for alpha releases (#2127)

### Changed

#### Refactors and Cleanups

- Clippy nightly: disable owned cmp, stop comparing bool using assert_eq (#2073, #2117)

### Fixed

#### Testing and Logging

- Remove broken ci-success job, which was skipping some required checks (#2084)
- Improve CI speed by removing redundant build jobs and Rust components (#2088)
- Fix a bad merge that was committed to main (#2085)

## [Zebra v1.0.0-alpha.7](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.7) - 2021-04-23

Zebra's latest alpha continues our work on NU5/Orchard, and fixes some security and protocol correctness issues.

Zebra now has best-effort support for Apple M1 builds, and logging to systemd-journald.

### Added

#### Network Upgrade 5

- Implement Sapling serialization in Transaction V5 (#1996, #2017, #2020, #2021, #2057)
- Draft RFC: Treestate management (#983)

#### Configuration and Logging

- Stop requiring a port for Zcash listener addresses (#2043)
  - use the default port if there is no configured port
- Add journald support through tracing-journald (#2034)
  - Zebra does not have any journald integration tests, so we will support it on a best-effort basis

#### Testing and Bug Reports

- Benchmark Block struct serialization code (#2018)
- Add the new commit count and git hash to the version in bug reports (#2038)
- Add branch, commit time, and build target to the panic metadata (#2028)
- Automatically update app version from crate version (#2028)

### Changed

#### Supported Platforms and Dependencies

- Update dependencies to support Apple M1 (#2026)
  - Zebra does not have any Apple M1 CI, so we will support it on a best-effort basis
- Bump ripemd160 from 0.8.0 to 0.9.1 and remove trait import (#2027)
- Update to vergen 5 (#2029)

#### Refactors and Cleanups

- Refactor and document correctness for std::sync::Mutex in ErrorSlot (#2032)
- Refactor and document correctness for std::sync::Mutex<AddressBook> (#2033)
- Make Zcash string serialization consistent with deserialization (#2053)
- clippy: make to_* methods take self by value (#2006)

#### Testing

- Speedup proptests for Chain struct in zebra-state (#2012)

### Fixed

- Stop assuming there will always be a `.git` directory during builds (#2037)
- Clarify CandidateSet state diagram (#2036)

#### Network Protocol

- Stop panicking when Zebra sends a reject without extra data (#2016)
- Switch to an async mutex for handshake nonces (#2031)

#### Testing and Logging

- Fix a test failure due to ' debug format changes in Rust (#2014)
- Fix Windows CI LLVM paths (#2026)
- Clarify a duplicate log message (#2054)

### Security

#### Network

- Avoid a single peer providing a majority of Zebra's peer addresses (#2004)
- Make sure handshake version negotiation always has a timeout (#2008)
- Make sure each peer heartbeat has a timeout (#2009)

#### Memory Usage

- Implement vector deserialisation limits for new Transaction::V5 types (#1996)



## [Zebra v1.0.0-alpha.6](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.6) - 2021-04-09

Zebra's latest alpha includes more validation of pre-NU5 consensus rules, continues our work on NU5/Orchard, and fixes some security and protocol correctness issues.

The Zebra project now has a [Code of Conduct](https://github.com/ZcashFoundation/zebra/blob/main/CODE_OF_CONDUCT.md).

### Added

- Design for Transaction V5 (#1886)
  - Make shielded data and spends generic over Transaction V4 and V5 (#1946, #1989)
- Async batching for:
  - Sprout `JoinSplit` signatures (#1952)
  - Sapling `Spend` and `Output` Groth16 proofs (#1713)
- Enable `Joinsplit` and `Spend` spend auth sighash verification (#1940)
- Randomised property tests for `InventoryHash` and `MetaAddr` (#1985)

#### Documentation

- Update the RFC process to include draft RFCs (#1962)
  - Merge some open RFCs as drafts (#1006, #1007, #1063, #1129)
- Add a fast start option to the Zebra Client RFC (#1969)
- Document that Zebra's mandatory checkpoint can change (#1935)

### Changed
- Refactor the Block Commitment field based on ZIP-244 (#1957, #1978, #1988)

### Fixed
- Stop ignoring inbound message errors and handshake timeouts (#1950)
- Don't send a useless heartbeat when the peer connection is already closing (#1950)

### Security

- Reduce deserialized memory usage for malicious blocks (#1920, #1977)
- Ensure that new MetaAddr fields are sanitized (#1942)
- Fix a deadlock between the peer address crawler and peer dialer when all connections fail (#1950)
- Avoid starvation of handshakes and crawling under heavy syncer load (#1950)
- Avoid starvation of request cancellations and timeouts under heavy peer response load (#1950)
- Async network code correctness documentation (#1954, #1972)

## [Zebra v1.0.0-alpha.5](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.5) - 2021-03-23

Zebra's latest alpha checkpoints on Canopy activation, continues our work on NU5, and fixes a security issue.

Some notable changes include:

### Added
- Log address book metrics when PeerSet or CandidateSet don't have many peers (#1906)
- Document test coverage workflow (#1919)
- Add a final job to CI, so we can easily require all the CI jobs to pass (#1927)

### Changed
- Zebra has moved its mandatory checkpoint from Sapling to Canopy (#1898, #1926)
  - This is a breaking change for users that depend on the exact height of the mandatory checkpoint.

### Fixed
- tower-batch: wake waiting workers on close to avoid hangs (#1908)
- Assert that pre-Canopy blocks use checkpointing (#1909)
- Fix CI disk space usage by disabling incremental compilation in coverage builds (#1923)

### Security
- Stop relying on unchecked length fields when preallocating vectors (#1925)

## [Zebra v1.0.0-alpha.4](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.4) - 2021-03-17

Zebra's latest alpha starts our work on NU5 and fixes several security issues.

Some notable changes include:

### Added

#### Network Upgrade 5

- Add some NU5 constants to zebra (#1823)
- Start work on transaction version 5 (#1824)

#### Metrics

- Add Grafana dashboards (#1830)
- Add Zebra version info to metrics (#1900)
- Add message type tag to message metrics (#1900)

### Changed

- [ZIP-215 Explicitly Defining and Modifying Ed25519 Validation Rules](https://github.com/zcash/zips/blob/master/zip-0215.rst) (#1811)
- Metrics renaming to enable node interoperability (#1900)
  - Renaming metrics breaks existing Grafana configs
- Rename config network.new_peer_interval to crawl_new_peer_interval (#1855)
  - This change includes a backwards-compatibility alias, so existing configs do not need to be updated

### Fixed

#### Code Style

- Apply nightly clippy suggestions (#1834)

#### Documentation

- Re-enable [zebra.zfnd.org](https://zebra.zfnd.org/) deployment (#1792)
- Document and log trailing message bytes (#1888)
- Move design/data-flow to rfcs/drafts (#1825)
- Document how inbound connections are added to the CandidateSet (#1852)

#### Hangs and Panics

- Fix a peer DNS resolution edge case (#1796)
- Stop sending blocks and transactions on the first error (#1818)
- Revert a connection refactor that caused frequent hangs (#1803)

#### Testing

- Avoid acceptance test port conflicts (#1812)
- Re-enable the checkpoint verifier restart tests (#1837)
- Adjust the crawl interval and acceptance test timeout (#1878)
- Explicitly auto-delete additional cache disks (#1859)

### Security

- Reduce inbound concurrency to limit memory usage (#1881)
- Verify proof-of-work in the checkpoint verifier (#1882)
- Implement outbound connection rate limiting (#1855)
- Document that the configured Zcash listener IP address is advertised to remote peers (#1891)

## [Zebra v1.0.0-alpha.3](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.3) - 2021-02-23

Zebra's latest alpha brings multiple reliability and stability improvements for node startup, long-running syncs, and testing.

Some notable changes include:

### Added
- Add beta rust to CI (#1725)
- Add Usability Testing Plan GitHub issue template (#1519)
- Add Release Checklist GitHub pull request template (#1717)

### Changed
- Compute the network message body length to reduce heap allocations (#1773)
- Re-enable the macOS conflict acceptance tests (#1778)
- Re-enable coverage CI (#1758, #1787)
- Disable fail-fast in the CI test job (#1776)
- Rename responsible_disclosure.md to SECURITY.md (#1747)

### Removed
- Disable unreliable testnet large sync test (#1789)

### Fixed

#### Hangs and Panics
- Refactor `connection.rs` to make `fail_with` panics impossible (#1721)
- Stop ignoring failed peer addresses (#1709)
- Retry initial peer DNS resolution on failure (#1762)
- Update tower-batch semaphore implementation (#1764)
- Use ready! in ChainVerifier::poll_ready (#1735)
- Use CallAllUnordered in peer_set::add_initial_peers (#1734)

#### Testing
- Bump CI build and test timeouts to 60 minutes (#1757)
- Run CI workflow on push to main & manual request (#1748)
- Set ZEBRA_SKIP_NETWORK_TESTS using Windows syntax (#1782)
- Fix Windows build failures due to disk space (#1726)
- Fix acceptance test timeouts, cleanup, and diagnostics (#1736, #1766, #1770, #1777)

#### Logging and Metrics
- Update PeerSet metrics after every change (#1727)
- Log initial peer connection failures (#1763)

## [Zebra v1.0.0-alpha.2](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.2) - 2021-02-09

Zebra's latest alpha brings multiple reliability and stability improvements for node startup, node shutdown, and long-running syncs.

Some notable changes include:

### Added
- Asynchronous Groth16 verification (#830)
- Security disclosure principles (#1650)

### Changed
- Document that connect\_isolated only works on mainnet (#1693)
- Document the impact of the redjubjub channel bound (#1682)
- Log when the syncer awaits peer readiness (#1714)

### Fixed
- Fix shutdown panics (#1637)
- Add hints to port conflict and lock file panics (#1535)
- Perform DNS seeder lookups concurrently, and add timeouts (#1662)
- Avoid buffer slot leaks in the Inbound service (#1620)
- Avoid a buffer slot leak by removing CallAllUnordered (#1705)
- Avoid future buffer slot leaks in ChainVerifier (#1700)
- Limit inbound download and verify queue (#1622)
- Increase a tower-batch queue bound (#1691)
- Fix a f64::NAN metrics sentinel (#1642)
- Actually use `VerifyCheckpointError::CommitFinalized` (#1706)

## [Zebra v1.0.0-alpha.1](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.1) - 2021-01-30

Zebra's second alpha brings multiple reliability and stability improvements for long-running syncs.

We've resolved known panics during syncing, and reduced the number of sync hangs.

Some notable changes include:

### Added
- Add peer set tracing (#1468)
- Add Sentry support behind a feature flag (#1461)
- Log configured network in every log message (#1568)

### Changed
- Export new precompute api in zebra-script (#1493)
- Rewrite peer block request hander to match the zcashd implementation (#1518)

### Fixed
- Avoid panics when there are multiple failures on the same connection (#1600)
- Add sync and inbound timeouts to prevent hangs (#1586)
- Fix Zebra versions so all crates are on the 1.0.0-alpha series (#1488)
- Make 'cargo run' use 'zebrad' rather than failing (#1569)
- Panic if the lookahead limit is misconfigured (#1589)
- Recommend using --locked with 'cargo install' (#1490)
- Simplify C++ compiler dependency in the README (#1498)
- Stop failing acceptance tests if their directories already exist (#1588)
- Stop panicking when ClientRequests return an error (#1531)
- Upgrade to tokio 0.3.6 to avoid a time wheel panic (#1583, #1511)

Currently, Zebra does not validate all the Zcash consensus rules.

## [Zebra v1.0.0-alpha.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.0) - 2021-01-30

Zebra first alpha release 🎉

The goals of this release are to:
- participate in the Zcash network,
- replicate the Zcash chain state,
- implement the Zcash proof of work consensus rules, and
- sync on Mainnet under excellent network conditions.

Currently, Zebra does not validate all the Zcash consensus rules.
It may be unreliable on Testnet, and under less-than-perfect
network conditions.
