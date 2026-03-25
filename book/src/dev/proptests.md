# Randomised Property Testing in Zebra

Zebra uses the [proptest](https://docs.rs/proptest/) crate for randomised property testing.

Most types in `zebra-chain` have an `Arbitrary` implementation, which generates randomised test cases.

We try to derive `Arbitrary` impls whenever possible, so that they automatically update when we make structural changes.
To derive, add the following attribute to the struct or enum:

```rust
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
struct Example(u32);
```

When we want to use those `Arbitrary` impls in proptests in other crates, we use the `proptest-impl` feature as a dev dependency:

1. in `zebra-chain`: make the `Arbitrary` impl depend on `#[cfg(any(test, feature = "proptest-impl"))]`
2. in the other crate: add zebra-chain as a dev dependency: `zebra-chain = { path = "../zebra-chain", features = ["proptest-impl"] }`

If we need to add another dependency as part of the `proptest-impl` feature:

1. Add the crate name to the list of crates in the `proptest-impl` `features` entry
2. Add the crate as an optional `dependencies` entry
3. Add the crate as a required `dev-dependencies` entry

For an example of these changes, see [PR 2070](https://github.com/ZcashFoundation/zebra/pull/2070/files).
