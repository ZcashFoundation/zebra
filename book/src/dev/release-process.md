# Zebra versioning and releases

This document contains the practices that we follow to provide you with a leading-edge application, balanced with stability.
We strive to ensure that future changes are always introduced in a predictable way.
We want everyone who depends on Zebra to know when and how new features are added, and to be well-prepared when obsolete ones are removed.

Before reading, you should understand [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and how a [Trunk-based development](https://www.atlassian.com/continuous-delivery/continuous-integration/trunk-based-development) works

<a id="versioning"></a>

## Zebra versioning

Zebra version numbers show the impact of the changes in a release. They are composed of three parts: `major.minor.patch`.
For example, version `3.1.11` indicates major version 3, minor version 1, and patch level 11.

The version number is incremented based on the level of change included in the release.

<div class="alert pre-release">

**NOTE**: <br />
As Zebra is in a `pre-release` state (is unstable and might not satisfy the intended compatibility requirements as denoted by its associated normal version).
The pre-release version is denoted by appending a hyphen and a series of dot separated identifiers immediately following the patch version.

</div>

| Level of change | Details                                                                                                                                                                                                                                                              |
| :-------------- | :------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Major release   | Contains significant new features, and commonly correspond to network upgrades; some technical assistance may be needed during the update. When updating to a major release, you may need to follow the specific upgrade instructions provided in the release notes. |
| Minor release   | Contains new smaller features. Minor releases should be fully backward-compatible. No technical assistance is expected during update. If you want to use the new features in a minor release, you might need to follow the instructions in the release notes.        |
| Patch release   | Low risk, bug fix release. No technical assistance is expected during update.                                                                                                                                                                                        |

<a id="supported-releases"></a>

### Supported Releases

Every Zebra version released by the Zcash Foundation is supported up to a specific height. Currently we support each version for about **16 weeks** but this can change from release to release.

When the Zcash chain reaches this end of support height, `zebrad` will shut down and the binary will refuse to start.

Our process is similar to `zcashd`: <https://zcash.github.io/zcash/user/release-support.html>

Older Zebra versions that only support previous network upgrades will never be supported, because they are operating on an unsupported Zcash chain fork.

<a id="updating"></a>

### Supported update paths

You can update to any version of Zebra, provided that the following criteria are met:

- The version you want to update _to_ is supported.
- The version you want to update _from_ is within one major version of the version you want to upgrade to.

See [Keeping Up-to-Date](guide/updating "Updating your projects") for more information about updating your Zebra projects to the most recent version.

<a id="previews"></a>

### Preview releases

We let you preview what's coming by providing Release Candidate \(`rc`\) pre-releases for some major releases:

| Pre-release type  | Details                                                                                                                                                                |
| :---------------- | :--------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Beta              | The release that is under active development and testing. The beta release is indicated by a release tag appended with the `-beta` identifier, such as `8.1.0-beta.0`. |
| Release candidate | A release for final testing of new features. A release candidate is indicated by a release tag appended with the `-rc` identifier, such as version `8.1.0-rc.0`.       |

### Distribution tags

Zebra's tagging relates directly to versions published on Docker. We will reference these [Docker Hub distribution tags](https://hub.docker.com/r/zfnd/zebra/tags) throughout:

| Tag    | Description                                                                                         |
| :----- | :-------------------------------------------------------------------------------------------------- |
| latest | The most recent stable version.                                                                     |
| beta   | The most recent pre-release version of Zebra for testing. May not always exist.                     |
| rc     | The most recent release candidate of Zebra, meant to become a stable version. May not always exist. |

### Feature Flags

To keep the `main` branch in a releasable state, experimental features must be gated behind a [Rust feature flag](https://doc.rust-lang.org/cargo/reference/features.html).
Breaking changes should also be gated behind a feature flag, unless the team decides they are urgent.
(For example, security fixes which also break backwards compatibility.)

<a id="frequency"></a>

## Release frequency

We work toward a regular schedule of releases, so that you can plan and coordinate your updates with the continuing evolution of Zebra.

<div class="alert is-helpful">

Dates are offered as general guidance and are subject to change.

</div>

In general, expect the following release cycle:

- A major release for each network upgrade, whenever there are breaking changes to Zebra (by API, severe bugs or other kind of upgrades)
- Minor releases for significant new Zebra features or severe bug fixes
- A patch release around every 6 weeks

This cadence of releases gives eager developers access to new features as soon as they are fully developed and pass through our code review and integration testing processes, while maintaining the stability and reliability of the platform for production users that prefer to receive features after they have been validated by Zcash and other developers that use the pre-release builds.

<a id="deprecation"></a>

## Deprecation practices

Sometimes "breaking changes", such as the removal of support for RPCs, APIs, and features, are necessary to:

- add new Zebra features,
- improve Zebra performance or reliability,
- stay current with changing dependencies, or
- implement changes in the \(blockchain\) itself.

To make these transitions as straightforward as possible, we make these commitments to you:

- We work hard to minimize the number of breaking changes and to provide migration tools, when possible
- We follow the deprecation policy described here, so you have time to update your applications to the latest Zebra binaries, RPCs and APIs
- If a feature has critical security or reliability issues, and we need to remove it as soon as possible, we will explain why at the top of the release notes

To help ensure that you have sufficient time and a clear path to update, this is our deprecation policy:

| Deprecation stages | Details                                                                                                                                                                                                                                                                                                                                                                                |
| :----------------- | :------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Announcement       | We announce deprecated RPCs and features in the [change log](https://github.com/ZcashFoundation/zebra/blob/main/CHANGELOG.md "Zebra change log"). When we announce a deprecation, we also announce a recommended update path.                                                                                                                                                          |
| Deprecation period | When a RPC or a feature is deprecated, it is still present until the next major release. A deprecation can be announced in any release, but the removal of a deprecated RPC or feature happens only in major release. Until a deprecated RPC or feature is removed, it is maintained according to the Tier 1 support policy, meaning that only critical and security issues are fixed. |
| Rust APIs          | The Rust APIs of the Zebra crates are currently unstable and unsupported. Use the `zebrad` commands or JSON-RPCs to interact with Zebra.                                                                                                                                                                                                                                               |

<a id="process"></a>

## Release candidate & release process

Our release checklist is available as a template, which defines each step our team needs to follow to create a new pre-release or release, and to also build and push the binaries to the official channels [Release Checklist Template](https://github.com/ZcashFoundation/zebra/blob/main/.github/PULL_REQUEST_TEMPLATE/release-checklist.md).
