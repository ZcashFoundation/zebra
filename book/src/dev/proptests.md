# Randomised Property Testing in Zebra

Zebra uses the [proptest](https://docs.rs/proptest/) crate for randomised property testing.

Most types in `zebra-chain` have an `Arbitrary` implementation, which generates randomised test cases.

When we want to use those `Arbitrary` impls in proptests in other crates, we use the `proptest-impl` feature as a dev dependency:
* in `zebra-chain`: make the `Arbitrary` impl depend on `#[cfg(any(test, feature = "proptest-impl"))]`
* in the other crate: add zebra-chain as a dev dependency: `zebra-chain = { path = "../zebra-chain", features = ["proptest-impl"] }`
