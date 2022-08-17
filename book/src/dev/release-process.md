# Zebra versioning and releases

This document contains the practices that we follow to provide you with a leading-edge application, balanced and with stability.
We strive to ensure that future changes are always introduced in a predictable way.
We want everyone who depends on Zebra to know when and how new features are added, and to be well-prepared when obsolete ones are removed.

<a id="versioning"></a>

## Zebra versioning

Zebra version numbers indicate the level of changes that are introduced by the release.
This use of [semantic versioning](https://semver.org/ "Semantic Versioning Specification") helps you understand the potential impact of updating to a new version.

Zebra version numbers have three parts: `major.minor.patch`.
For example, version `3.1.11` indicates major version 3, minor version 1, and patch level 11.

The version number is incremented based on the level of change included in the release.

<div class="alert pre-release">

**NOTE**: <br />
As Zebra is in a `pre-release` state (is unstable and might not satisfy the intended compatibility requirements as denoted by its associated normal version).
The pre-release version is denoted by appending a hyphen and a series of dot separated identifiers immediately following the patch version.

</div>

| Level of change | Details |
|:---             |:---     |
| Major release   | Contains significant new features, and commonly correspond to network upgrades; some technical assistance is expected during the update. When updating to a new major release, you might need to <fill-with-things-the-user-might-need-to-do>. |
| Minor release   | Contains new smaller features. Minor releases are meant to be fully backward-compatible, but may include breaking changes to public APIs. No technical assistance is expected during update. |
| Patch release   | Low risk, bug fix release. No technical assistance is expected during update. |

<a id="updating"></a>

### Supported update paths

You can update to any version of Zebra, provided that the following criteria are met:

* The version you want to update *to* is supported.
* The version you want to update *from* is within one major version of the version you want to
    upgrade to.

See [Keeping Up-to-Date](guide/updating "Updating your projects") for more information about updating your Zebra projects to the most recent version.

<a id="previews"></a>

### Preview releases

We let you preview what's coming by providing "Next" and Release Candidates \(`rc`\) pre-releases for each major and minor release:

| Pre-release type  | Details |
|:---               |:---     |
| Next              | The release that is under active development and testing. The next release is indicated by a release tag appended with the `-next` identifier, such as  `8.1.0-next.0`.      |
| Release candidate | A release that is feature complete and in final testing. A release candidate is indicated by a release tag appended with the `-rc` identifier, such as version `8.1.0-rc.0`. |

The latest `next` or `rc` pre-release version of the documentation is available at [next.Zebra.io](https://next.Zebra.io).

<a id="frequency"></a>

## Release frequency

We work toward a regular schedule of releases, so that you can plan and coordinate your updates with the continuing evolution of Zebra.

<div class="alert is-helpful">

Dates are offered as general guidance and are subject to change.

</div>

In general, expect the following release cycle:

* A major once a network upgrade is required
* 1-3 minor releases for each major release
* A patch release build almost semiweekly

This cadence of releases gives eager developers access to new features as soon as they are fully developed and pass through our code review and integration testing processes, while maintaining the stability and reliability of the platform for production users that prefer to receive features after they have been validated by Zcash and other developers that use the pre-release builds.

<a id="deprecation"></a>

## Deprecation practices

Sometimes "breaking changes", such as the removal of support for select APIs and features, are necessary to innovate and stay current with new best practices, changing dependencies, or changes in the \(blockchain\) itself.

To make these transitions as straightforward as possible, we make these commitments to you:

* We work hard to minimize the number of breaking changes and to provide migration tools, when possible
* We follow the deprecation policy described here, so you have time to update your applications to the latest APIs and best practices

To help ensure that you have sufficient time and a clear path to update, this is our deprecation policy:

| Deprecation stages | Details |
|:---                |:---     |
| Announcement       | We announce deprecated APIs and features in the [change log](https://github.com/ZcashFoundation/zebra/blob/main/CHANGELOG.md "Zebra change log"). Deprecated APIs appear in the [documentation](missing-link) with ~~strikethrough~~. When we announce a deprecation, we also announce a recommended update path. |
| Deprecation period | When an API or a feature is deprecated, it is still present in the next major release. After that, deprecated APIs and features are candidates for removal. A deprecation can be announced in any release, but the removal of a deprecated API or feature happens only in major release. Until a deprecated API or feature is removed, it is maintained according to the Tier 1 support policy, meaning that only critical and security issues are fixed. |
| crate dependencies | We only make crate dependency updates that require changes to your applications in a major release. In minor releases, we update peer dependencies by expanding the supported versions, but we do not require projects to update these dependencies until a future major version. This means that during minor Zebra releases, crate dependency updates within Zebra applications and libraries are optional. |

<a id="process"></a>

## Release candidate & release process

Identify the commit from which the release stabilization branch will be made.
Release stabilization branches are used so that development can proceed
unblocked on the `master` branch during the release candidate testing and
bug-fixing process. By convention, release stabilization branches are named
`version-X.Y.0` where `X` and `Y` are the major and minor versions for the
release.

### Create the release stabilization branch

**\<TBD\>**

### Create the release candidate branch

**\<TBD\>**

### Make a tag for the tip of the release candidate branch

**\<TBD\>**

## Make and deploy deterministic builds

**\<TBD\>**

## Add release notes to GitHub

## Go/No-Go Status: ⚠️

* `zebrad` Functionality
  * `zebrad` can sync to mainnet tip
    * ⚠️ under excellent network conditions (within 2 - 5 hours)
    * _reasonable and sub-optimal network conditions are not yet supported_
  * `zebrad` can stay within a few blocks of the mainnet tip after the initial sync
    * ⚠️ under excellent network conditions
    * _reasonable and sub-optimal network conditions are not yet supported_
  * ✅ `zebrad` can validate proof of work
  * ✅ `zebrad` can validate the transaction merkle tree
  * ⚠️ `zebrad` can serve blocks to peers
  * ✅ The hard-coded [checkpoint lists](https://github.com/ZcashFoundation/zebra/tree/main/zebra-consensus/src/checkpoint) are up-to-date
* `zebrad` Performance
  * ✅ `zebrad` functionality works on platforms that meet its system requirements
* Testing
  * ⚠️ CI Passes
    * ✅  Unit tests pass reliably
    * ✅  Property tests pass reliably
    * ⚠️ Acceptance tests pass reliably
  * ✅ Each Zebra crate [builds individually](https://github.com/ZcashFoundation/zebra/issues/1364)
* Implementation and Launch
  * ✅ All [release blocker bugs](https://github.com/ZcashFoundation/zebra/issues?q=is%3Aopen+is%3Aissue+milestone%3A%22First+Alpha+Release%22+label%3AC-bug) have been fixed
  * ✅ The list of [known serious issues](https://github.com/ZcashFoundation/zebra#known-issues) is up to date
  * ✅ The Zebra crate versions are up to date
  * ⚠️ Users can access [the documentation to deploy `zebrad` nodes](https://github.com/ZcashFoundation/zebra#getting-started)
* User Experience
  * ✅ Build completes within 40 minutes in Zebra's CI
    * ✅ Unused dependencies have been removed (use `cargo-udeps`)
  * ✅ `zebrad` executes normally
    * ✅ `zebrad`'s default logging works reasonably well in a terminal
    * ✅ panics, error logs, and warning logs are rare on mainnet
    * ✅ known panics, errors and warnings have open tickets

## Post Release Task List

### Merge the release stabilization branch

<!-- links -->

<!-- external links -->

<!-- end links -->

@reviewed **\<add-date-here\>**
