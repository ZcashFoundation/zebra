use super::*;
use std::collections::HashSet;

fn hash(byte: u8) -> transaction::Hash {
    let mut bytes = [0u8; 32];
    bytes[0] = byte;
    transaction::Hash::from(bytes)
}

fn outpoint(hash: transaction::Hash) -> transparent::OutPoint {
    transparent::OutPoint::from_usize(hash, 0)
}

#[test]
fn remove_all_drops_stale_dependents() {
    let mut deps = TransactionDependencies::default();
    let parent = hash(1);
    let child = hash(2);

    deps.add(child, vec![outpoint(parent)]);
    assert!(deps.direct_dependents(&parent).contains(&child));

    // Removing the child first should also erase it from the parent's dependents list.
    let removed = deps.remove_all(&child);
    assert!(removed.is_empty());
    assert!(deps.direct_dependents(&parent).is_empty());

    // Removing the parent afterwards should not re-discover the child.
    let removed_parent = deps.remove_all(&parent);
    assert!(removed_parent.is_empty());
    assert!(deps.direct_dependents(&parent).is_empty());
}

#[test]
fn remove_all_returns_transitive_dependents() {
    let mut deps = TransactionDependencies::default();
    let parent = hash(10);
    let child = hash(11);
    let grandchild = hash(12);

    deps.add(child, vec![outpoint(parent)]);
    deps.add(grandchild, vec![outpoint(child)]);

    let removed = deps.remove_all(&parent);
    let expected: HashSet<_> = [child, grandchild].into_iter().collect();
    assert_eq!(removed, expected);
    assert!(deps.direct_dependents(&parent).is_empty());
    assert!(deps.direct_dependents(&child).is_empty());
    assert!(deps.direct_dependencies(&grandchild).is_empty());
}

#[test]
fn remove_parent_with_multiple_children() {
    let mut deps = TransactionDependencies::default();
    let parent = hash(30);
    let children = [hash(31), hash(32), hash(33)];

    for child in children {
        deps.add(child, vec![outpoint(parent)]);
    }

    let removed = deps.remove_all(&parent);
    let expected: HashSet<_> = children.into_iter().collect();
    assert_eq!(removed, expected);
    assert!(deps.direct_dependents(&parent).is_empty());
}

#[test]
fn remove_child_with_multiple_parents() {
    let mut deps = TransactionDependencies::default();
    let parents = [hash(40), hash(41)];
    let child = hash(42);

    deps.add(
        child,
        parents.iter().copied().map(outpoint).collect::<Vec<_>>(),
    );

    let removed = deps.remove_all(&child);
    assert!(removed.is_empty());
    for parent in parents {
        assert!(deps.direct_dependents(&parent).is_empty());
    }
}

#[test]
fn remove_from_complex_graph() {
    let mut deps = TransactionDependencies::default();
    let a = hash(50);
    let b = hash(51);
    let c = hash(52);
    let d = hash(53);

    // A -> B, A -> C, and B -> D, C -> D
    deps.add(b, vec![outpoint(a)]);
    deps.add(c, vec![outpoint(a)]);
    deps.add(d, vec![outpoint(b), outpoint(c)]);

    let removed = deps.remove_all(&a);
    let expected: HashSet<_> = [b, c, d].into_iter().collect();
    assert_eq!(removed, expected);
    assert!(deps.dependents().is_empty());
    assert!(deps.dependencies().is_empty());
}

#[test]
fn clear_mined_dependencies_removes_correct_hash() {
    let mut deps = TransactionDependencies::default();
    let parent = hash(20);
    let child = hash(21);

    deps.add(child, vec![outpoint(parent)]);
    assert!(deps.direct_dependencies(&child).contains(&parent));

    let mined_ids: HashSet<_> = [parent].into_iter().collect();
    deps.clear_mined_dependencies(&mined_ids);

    assert!(deps.direct_dependents(&parent).is_empty());
    assert!(deps.direct_dependencies(&child).is_empty());
    assert!(deps.dependents().get(&parent).is_none());
}
