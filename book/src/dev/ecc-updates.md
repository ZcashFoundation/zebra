# Updating the ECC dependencies

Zebra relies on numerous Electric Coin Company ([ECC](https://electriccoin.co/)) dependencies, and updating them can be a complex task. This guide will help you navigate the process.

The main dependency that influences that is [zcash](https://github.com/zcash/zcash) itself. This is because [zebra_script](https://github.com/ZcashFoundation/zcash_script) links to specific files from it (zcash_script.cpp and all on which it depends). Due to the architecture of zcash, this requires linking to a lot of seemingly unrelated dependencies like orchard, halo2, etc (which are all Rust crates).

## Steps for upgrading

Let's dive into the details of each step required to perform an upgrade:

### Before starting

- Zebra developers often dismiss ECC dependency upgrade suggestions from dependabot. For instance, see [this closed PR](https://github.com/ZcashFoundation/zebra/pull/7745) in favor of the [5.7.0 zcashd upgrade PR](https://github.com/ZcashFoundation/zebra/pull/7784), which followed this guide.

- Determine the version of `zcashd` to use. This version will determine which versions of other crates to use. Typically, this should be a [tag](https://github.com/zcash/zcash/tags), but in some cases, it might be a reference to a branch (e.g., nu5-consensus) for testing unreleased developments.

- Upgrading the `zcash_script` crate can be challenging, depending on changes in the latest `zcashd` release. Follow the instructions in the project's [README](https://github.com/ZcashFoundation/zcash_script/blob/master/README.md) for guidance.

- Upgrade and release `zcash_script` before upgrading other ECC dependencies in Zebra.

### Upgrade versions

- Use the `cargo upgrade` command to upgrade all the ECC dependency versions in Zebra. For example, in [this PR](https://github.com/ZcashFoundation/zebra/pull/7784), the following command was used:

```sh
cargo upgrade --incompatible -p bridgetree -p incrementalmerkletree -p orchard -p zcash_primitives -p zcash_proofs -p zcash_address -p zcash_encoding -p zcash_note_encryption -p zcash_script
```

Notes:

- Insert all the crate names to be updated to the command.

- Use `crate-name@version` to upgrade to a specific version of that crate, instead of just the highest version.

- You need to have [cargo upgrade](https://crates.io/crates/cargo-upgrades) and [cargo edit](https://crates.io/crates/cargo-edit) installed for this command to work.

### Version consistency check

- Ensure that the crate versions in the `Cargo.toml` of the zcashd release, `Cargo.toml` of `zcash_script`, and the `Cargo.toml` files of Zebra crates are all the same. Version consistency is crucial.

### Build/Test zebra & fix issues

- Build zebra and make sure it compiles.

```sh
cargo build
```

- Test Zebra and make sure all test code compiles and all tests pass:

```sh
cargo test
```

- When upgrading, it's common for things to break, such as deprecated or removed functionality. Address these issues by referring to the broken dependency's changelog, which often provides explanations and workarounds.

- If you encounter issues that you can't resolve, consider reaching out to ECC team members who worked on the upgrade, as they may have more context.

### Check `deny.toml`

- Review Zebra's `deny.toml` file for potential duplicates that can be removed due to the upgrade. You may also need to add new entries to `deny.toml`.
- You can identify issues with the dependencies using `cargo deny check bans` command, need to have [cargo deny](https://crates.io/crates/cargo-deny) installed.
- Push your changes and let the CI identify any additional problems.

### Push the Pull Request (PR)

- Push the pull request with all the changes and ensure that the full CI process passes.
- Seek approval for the PR.
- Merge to `main` branch.
