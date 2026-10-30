# Updating Zcash protocol dependencies

Zebra shares transaction, proof, key, and script libraries with other Zcash
implementations. Review consensus validation and shared types alongside
upstream changes and the resolved dependency graph; a successful build does not
establish consensus compatibility.

## Dependency inventory

The root [`Cargo.toml`][workspace-manifest] holds most direct requirements;
member manifests select features and package aliases. The table lists version
requirements, not the resolved versions in `Cargo.lock`.

| Group | Direct dependencies and requirements |
| --- | --- |
| Transaction and protocol libraries | `zcash_primitives = "0.31.0"`, `zcash_proofs = "0.31.0"`, `zcash_protocol = "0.11.0"`, `zcash_transparent = "0.11.0"` |
| Addresses and keys | `zcash_address = "0.14.0"`, `zcash_keys = "0.17.0"` |
| Shielded protocols | `orchard = "0.16.0"`, `sapling-crypto = "0.9"`, `zcash_note_encryption = "0.5.1"` |
| Encoding and trees | `zcash_encoding = "0.5"`, `zcash_history = "0.6.0"`, `incrementalmerkletree = "0.9"` |
| Transparent scripts | `libzcash_script = "0.2"`, `zcash_script = "0.6"` |

The cryptographic dependency graph also includes `bellman`, `bls12_381`,
`equihash`, `ff`, `group`, `groth16`, `jubjub`, `reddsa`, `redjubjub`, and
`sinsemilla`. Several member manifests depend on `halo2_proofs = "0.4"` under
the local name `halo2`. Check those declarations as well as the workspace
requirements. The `halo2_gadgets` package is a transitive dependency.

The [script crate] uses `libzcash_script` for the default C++ interpreter from
`zcashd` and `zcash_script` for Rust types and interpreter support. Test changes
with the [`comparison-interpreter` feature](../user/install.md#cargo-features).

Match versions where crates exchange types or require compatible
implementations, not to a `zcashd` release's entire dependency list:
`libzcash_script` depends on `zcash_script`, not the Orchard or Sapling graph.

## Select and apply the update

1. Record the target releases and read their changelogs. Identify changes to
   serialization, consensus checks, feature defaults, proof verification, and
   public types. Apply the [consensus refactor checklist](consensus-refactors.md)
   when validation moves between Zebra and an upstream library.
2. Check the dependency requirements of the selected upstream releases. If
   script changes require a new `libzcash_script` or `zcash_script` release,
   coordinate that release in the [script repository] before selecting it in
   Zebra. Do not require a script release for an unrelated library update.
3. Update the relevant `[workspace.dependencies]` entries and any member-level
   aliases or feature selections. Do not replace inherited workspace
   requirements with independent per-crate versions.
4. Update `Cargo.lock` for the selected packages:

   ```sh
   cargo update -p orchard -p sapling-crypto \
     -p zcash_primitives -p zcash_proofs
   cargo tree --workspace --duplicates
   cargo tree --workspace --invert zcash_primitives
   ```

   Adjust the package list to the update. If more than one version of a
   package is present, use a version-qualified package specification such as
   `zcash_primitives@0.31.0` for an inverse tree. Review transitive changes in
   the lockfile; a targeted update can still update required dependencies.

5. Adapt callers without weakening validation, following the
   [consensus refactor checklist](consensus-refactors.md).

## Dependency policy and audits

The [deny policy] rejects unapproved duplicate versions, registries, Git
sources, and licenses. It also checks security advisories. Remove obsolete
exceptions when the update eliminates their dependency paths. Do not add a
broad `skip-tree` exception to hide an unexplained duplicate.

Install [cargo-deny] and run the default-feature checks from the workspace
root:

```sh
cargo deny --workspace check bans sources licenses advisories
```

For CI-only failures, reproduce the [lint feature matrix and allowances](continuous-integration.md#fixing-duplicate-dependencies-in-check-denytoml-bans)
rather than weakening the shared policy.

Zebra also stores [cargo-vet] review records in `supply-chain/`:

- `config.toml`: imported audit sources, package policy, and exemptions.
- `audits.toml`: local full-version and version-to-version audit records.
- `imports.lock`: the imported audit records used by the repository.

Run the audit check after updating the dependency graph:

```sh
cargo vet
```

For missing coverage, inspect the package with `cargo vet inspect`, or review
the version delta with `cargo vet diff`. After completing that review, record
the applicable criteria with `cargo vet certify`. These commands take the
package name and the version or version pair reported by the tool; see the
[cargo-vet command reference]. Commit the resulting supply-chain changes with
the dependency update. An exemption records missing audit coverage, not a
successful audit; document and review any required exemption explicitly.

## Validation and review

Run affected tests and feature combinations, including `comparison-interpreter`
for script changes, and the [CI checks](continuous-integration.md). For
consensus-sensitive updates, follow the [refactor validation requirements](consensus-refactors.md#reviewer-checklist).

The pull request must identify the upstream releases, explain necessary
version coupling and policy exceptions, and record test evidence. Keep
unrelated dependency changes out of the update so that reviewers can trace
behavior changes to their source.

[workspace-manifest]: https://github.com/ZcashFoundation/zebra/blob/main/Cargo.toml
[script crate]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-script/Cargo.toml
[script repository]: https://github.com/ZcashFoundation/zcash_script
[deny policy]: https://github.com/ZcashFoundation/zebra/blob/main/deny.toml
[cargo-deny]: https://embarkstudios.github.io/cargo-deny/
[cargo-vet]: https://mozilla.github.io/cargo-vet/
[cargo-vet command reference]: https://mozilla.github.io/cargo-vet/commands.html
