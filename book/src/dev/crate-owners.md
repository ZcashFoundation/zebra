# Zebra Crates

The Zebra project publishes around 20 crates to the Rust [crates.io website](https://crates.io).
Zcash Foundation crates are controlled by the [`ZcashFoundation/owners`](https://github.com/orgs/ZcashFoundation/teams/owners) GitHub team.

The latest list of Zebra and FROST crates is [available on crates.io](https://crates.io/teams/github:zcashfoundation:owners).

The Zebra repository can be used to publish the crates in this list that match these patterns:

- starts with `zebra` (including `zebrad` and the `zebra` placeholder)
- starts with `tower`

We also depend on these separate ZF crates:

- `zcash_script`
- `ed25519-zebra`

And these crates shared with ECC:

- `reddsa`
- `redjubjub`

## Logging in to crates.io

To publish a crate or change owners, you'll need to [log in to crates.io](https://doc.rust-lang.org/cargo/reference/publishing.html#before-your-first-publish) using `cargo login`.

When you create a token, give it an expiry date, and limit its permissions to the task you're doing. For example, if you're doing a release, create a token for releasing crates.

Tokens that allow changing owners should have the shortest expiry possible.

[Revoke the token](https://crates.io/me) after you're finished using it.

Here is an example login command:

```sh
$ cargo login
please paste the token found on https://crates.io/me below
...
       Login token for `crates.io` saved
```

## Publishing New Crates

We publish a new placeholder crate as soon as we have a good idea for a crate name.

Before starting with the publishing, please clone zebra and use the `main` branch to create the placeholder crate, you need `cargo release` installed in the system and be logged to crates.io with `cargo login`.

Next, execute the following commands to publish a new placeholder and set the owners:

```sh
cargo new new-crate-name
cd new-crate-name
cargo release publish --verbose --package new-crate-name --execute
cargo owner --add oxarbitrage
cargo owner --add conradoplg
cargo owner --add github:zcashfoundation:owners
```

## Changing Crate Ownership

crates.io has two kinds of owners: group owners and individual owners. All owners can publish and yank crates.
But [only individual owners can change crate owners](https://doc.rust-lang.org/cargo/reference/publishing.html#cargo-owner).

Zcash Foundation crates should have:

- at least 2 individual owners, who are typically engineers on the relevant project
- a group owner that contains everyone who can publish the crate

When an individual owner leaves the foundation, they should be [replaced with another individual owner](https://doc.rust-lang.org/cargo/reference/publishing.html#cargo-owner).

New crate owners should go to [crates.io/me](https://crates.io/me) to accept the invitation, then they will appear on the list of owners.

Here are some example commands for changing owners:

To change owners of deleted/placeholder Zebra crates:

```sh
$ mkdir placeholders
$ cd placeholders
$ for crate in tower-batch-cpu zebra zebra-cli zebra-client; do cargo new $crate; pushd $crate; cargo owner --add oxarbitrage; cargo owner --remove dconnolly; popd; done
     Created binary (application) `zebra-cli` package
~/zebra-cli ~
    Updating crates.io index
       Owner user oxarbitrage has been invited to be an owner of crate zebra-cli
    Updating crates.io index
       Owner removing ["dconnolly"] from crate zebra-cli
~
     Created binary (application) `zebra-client` package
~/zebra-client ~
    Updating crates.io index
       Owner user oxarbitrage has been invited to be an owner of crate zebra-client
    Updating crates.io index
       Owner removing ["dconnolly"] from crate zebra-client
~
...
```

To change owners of `zcash_script`:

```sh
$ git clone https://github.com/ZcashFoundation/zcash_script
$ cd zcash_script
$ cargo owner --add oxarbitrage
    Updating crates.io index
       Owner user oxarbitrage has been invited to be an owner of crate zcash_script
$ cargo owner --remove dconnolly
    Updating crates.io index
       Owner removing ["dconnolly"] from crate zcash_script
```

To change owners of current Zebra crates:

```sh
$ git clone https://github.com/ZcashFoundation/zebra
$ cd zebra
$ for crate in tower-* zebra*; do pushd $crate; cargo owner --add oxarbitrage; cargo owner --remove dconnolly; popd; done
~/zebra/tower-batch-control ~/zebra
    Updating crates.io index
       Owner user oxarbitrage already has a pending invitation to be an owner of crate tower-batch-control
    Updating crates.io index
       Owner removing ["dconnolly"] from crate tower-batch-control
~/zebra
~/zebra/tower-fallback ~/zebra
    Updating crates.io index
       Owner user oxarbitrage has been invited to be an owner of crate tower-fallback
    Updating crates.io index
       Owner removing ["dconnolly"] from crate tower-fallback
~/zebra
...
```
