//! Checks the Zebra book's JSON-RPC method table against the registered methods.

use std::collections::BTreeSet;

use crate::methods::METHODS;

const RPC_PAGE: &str = include_str!("../../../../book/src/user/rpc.md");

#[test]
fn book_lists_exactly_the_registered_rpc_methods() {
    let (_, methods_section) = RPC_PAGE
        .split_once("\n## Methods\n")
        .expect("book/src/user/rpc.md has a Methods section");
    // The section ends at the next level-two heading; its tables are under level-three headings.
    let methods_section = methods_section
        .split_once("\n## ")
        .map_or(methods_section, |(section, _)| section);
    let documented: BTreeSet<&str> = methods_section
        .lines()
        .filter_map(|line| line.strip_prefix("| `")?.split('`').next())
        .collect();
    let registered: BTreeSet<&str> = METHODS.keys().copied().collect();

    assert_eq!(
        documented, registered,
        "the Methods section of book/src/user/rpc.md must list exactly the registered RPC methods"
    );
}
