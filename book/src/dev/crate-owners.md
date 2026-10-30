# Zebra crates

The [workspace][workspace] publishes 12 crates:

- `zebrad`
- `zebra-chain`, `zebra-consensus`, `zebra-network`, `zebra-node-services`
- `zebra-rpc`, `zebra-script`, `zebra-state`, `zebra-test`, `zebra-utils`
- `tower-batch-control`, `tower-fallback`

Each of these crates has the following crates.io owners, verified on
October 24, 2026:

| Owner | Kind |
| --- | --- |
| `conradoplg` | Individual |
| `oxarbitrage` | Individual |
| `github:zcashfoundation:owners` | GitHub team |

The [owners team listing][team-crates] includes other Foundation projects.
Inspect the registry's live list before changing ownership:

```sh
cargo owner --list zebrad
```

## Logging in to crates.io

Normal releases use [Trusted Publishing][trusted-publishing]: the
[release workflow][release-workflow] obtains short-lived crates.io credentials
through GitHub Actions OpenID Connect and publishes from the approved source.

A crate owner must configure each crate's Trusted Publisher with repository
`ZcashFoundation/zebra`, workflow `release.yml`, and environment `release`.
Repository CI cannot inspect these owner-only bindings. Confirm them before the
first automated publication of a crate.

Manual ownership changes, authorized yanks, and exceptional publication require
a crates.io token. Create it in [account settings][account], restrict it to the
required crates and operation, and set the shortest practical expiry.

Authenticate interactively rather than putting the token in shell history:

```sh
cargo login
```

Revoke the token in account settings after the operation. Do not put registry
tokens in committed configuration, command output, or issue reports.

## Publishing new crates

Agree on the crate's purpose and name before publication, which a Git revert
cannot undo:

1. Add its manifest and workspace membership through review. Confirm its package
   name, license, repository metadata, dependencies, and publication settings.
2. Add its project mapping and initial changelog source to `.changie.yaml` and
   `.changes/` before generation. A project with no version history is rendered
   as only the shared header; do not add an existing crate without migrating its
   changelog history.
3. Review the version and changelog in the [release process](release-process.md).
   Arrange an authorized API-token first publication: crates.io requires the
   crate to exist before [Trusted Publishing][trusted-publishing] can be
   configured. Use the normal controller for subsequent releases.
4. Add the [required owners](#changing-crate-ownership), confirm acceptance and
   the live list, and configure Trusted Publishing before automated publication.

Never run a second publisher alongside the
[release controller](release-process.md#release-candidate--release-process).

## Changing crate ownership

Both individual and team owners can publish and yank versions. Only individual
owners can change the owner set; see [Cargo's ownership reference][cargo-owner].
Keep at least two individual owners and the Foundation team on each crate.

Invite replacements before removing a departing maintainer. Individual invitees
must accept on [crates.io][account] before they appear as owners. Set `CRATE`,
`NEW_OWNER`, and `OLD_OWNER` to the approved crate and GitHub logins:

```sh
cargo owner --list "${CRATE}"
cargo owner --add "${NEW_OWNER}" "${CRATE}"
cargo owner --add github:zcashfoundation:owners "${CRATE}"
```

After the replacement accepts, inspect the list and remove only the intended
former owner:

```sh
cargo owner --list "${CRATE}"
cargo owner --remove "${OLD_OWNER}" "${CRATE}"
cargo owner --list "${CRATE}"
```

These commands require no checkout, even for crates outside the workspace.

[workspace]: https://github.com/ZcashFoundation/zebra/blob/main/Cargo.toml
[team-crates]: https://crates.io/teams/github:zcashfoundation:owners
[trusted-publishing]: https://crates.io/docs/trusted-publishing
[release-workflow]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/release.yml
[account]: https://crates.io/me
[cargo-owner]: https://doc.rust-lang.org/cargo/reference/publishing.html#cargo-owner
