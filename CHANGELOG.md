# CHANGELOG

All notable changes to Zebra will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org).

## [Zebra 1.0.0-alpha.15](https://github.com/ZcashFoundation/zebra/releases/tag/v1.0.0-alpha.15) - 2021-08-TBD

Zebra's latest alpha contains the last of the changes to zebra chain state ahead of NU5 testnet activation and before concentrating on mempool work. The remaining NU5 validation will be completed prior to NU5 mainnet activation.

### Added

- Reject connections from outdated peers after network upgrade activation (#2519)

#### Network Upgrade 5

- ZIP-209: Prohibit Negative Shielded Chain Value Pool Balances Partial Implementation (#2546, #2554, #2561, #2569, #2576, #2577, #2578, #2596)
- ZIP-221: FlyClient - Consensus-Layer Changes Implementation (#2531, #2553, #2583)
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
