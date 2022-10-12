# Zebra Branching and Versioning

This guide explains how the Zebra team manages branches and how those branches relate to
merging PRs and publishing releases. Before reading, you should understand
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) and how a [Trunk-based development](https://www.atlassian.com/continuous-delivery/continuous-integration/trunk-based-development) works

## Distribution tags

Zebras's branching relates directly to versions published on Docker. We will reference these [Docker Hub
distribution tags](https://hub.docker.com/r/zfnd/zebra/tags) throughout:

| Tag    | Description                                                                       |
|--------|-----------------------------------------------------------------------------------|
| latest | The most recent stable version.                                                   |
| beta   | The most recent pre-release version of Zebra for testing. May not always exist.   |
| rc     | The most recent release candidate of Zebra, meant to become a stable version.     |

## Branch naming

Zebra's core “trunk” branch is `main`. This branch always represents the absolute latest changes. The
code on `main` always represents a pre-release version, often published with the `beta` tag on Docker Hub.

For each minor and major version increment, a new branch is created. These branches use a naming
scheme matching `\d+\.\d+\.x` and receive subsequent patch changes for that version range. For
example, the `10.2.x` branch represents the latest patch changes for subsequent releases starting
with `10.2.`. The version tagged on Docker Hub as `latest` will always correspond to such a branch,
referred to as the **active patch branch**.

### Feature freeze and release candidates

Before publishing minor and major versions as `latest` on Docker Hub, they go through a feature freeze and
a release candidate (RC) phase.

**Feature freeze** means that `main` is forked into a branch for a specific version, with no
additional features permitted before releasing as `latest` to Docker Hub. This branch becomes the **active
RC branch**. Upon branching, the `main` branch increments to the next minor or major pre-release
version. One week after feature freeze, the first RC is published with the `next` tag on Docker Hub from
the active RC branch. Patch bug fixes continue to merge into `main`, the active RC branch, and
the active patch branch during this entire period.

One to three weeks after publishing the first RC, the active RC branch is published as `latest` on
Docker Hub and the branch becomes the active patch branch. At this point there is no active RC branch until
the next minor or major release.
