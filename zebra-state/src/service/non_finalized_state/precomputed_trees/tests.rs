//! Tests for precomputed note commitment tree updates.

use std::sync::Arc;

use zebra_chain::{
    orchard::tree::{NoteCommitmentTree, NoteCommitmentUpdate},
    parallel::tree::{NoteCommitmentTrees, PrecomputedTreeUpdate},
    transaction,
};

use super::{
    TreeUpdates, PRECOMPUTED_TREE_UPDATE_NOTE_COMMITMENTS, PRECOMPUTED_TREE_UPDATE_TRANSACTIONS,
};

/// Returns the ID of test transaction `transaction`.
fn id(transaction: u8) -> transaction::Hash {
    transaction::Hash([transaction; 32])
}

/// Returns the two note commitments of test transaction `transaction`.
fn note_commitments(transaction: u8) -> Vec<NoteCommitmentUpdate> {
    (0..2)
        .map(|action| NoteCommitmentUpdate::from(u64::from(transaction) * 10 + action))
        .collect()
}

/// Returns a tree with `leaves` note commitments.
fn tree(leaves: u64) -> Arc<NoteCommitmentTree> {
    let mut tree = NoteCommitmentTree::default();
    for leaf in 0..leaves {
        tree.append(NoteCommitmentUpdate::from(1_000 + leaf))
            .expect("the tree has room");
    }
    Arc::new(tree)
}

/// Returns the precomputed updates in `updates`.
fn precomputed(updates: &TreeUpdates) -> Vec<Arc<PrecomputedTreeUpdate>> {
    updates
        .updates
        .iter()
        .map(|(_transactions, update)| update.clone())
        .collect()
}

/// Returns every order of every one, two and three of three transactions.
fn orders_of([first, second, third]: [u8; 3]) -> [Vec<u8>; 15] {
    [
        vec![first],
        vec![second],
        vec![third],
        vec![first, second],
        vec![second, first],
        vec![first, third],
        vec![third, first],
        vec![second, third],
        vec![third, second],
        vec![first, second, third],
        vec![first, third, second],
        vec![second, first, third],
        vec![second, third, first],
        vec![third, first, second],
        vec![third, second, first],
    ]
}

/// Checks that `updates` has the update a block makes with the transactions of each of `orders`,
/// and no others.
fn check_updates(updates: &TreeUpdates, parent: &Arc<NoteCommitmentTree>, orders: &[Vec<u8>]) {
    let precomputed = precomputed(updates);
    assert_eq!(precomputed.len(), orders.len());

    for order in orders {
        let note_commitments: Vec<_> = order
            .iter()
            .flat_map(|&transaction| note_commitments(transaction))
            .collect();

        let (found, _subtree) =
            PrecomputedTreeUpdate::find(&precomputed, parent, &note_commitments)
                .unwrap_or_else(|| panic!("the order {order:?} has an update"));
        let (computed, _subtree) = NoteCommitmentTrees::update_orchard_note_commitment_tree(
            parent.clone(),
            note_commitments,
        )
        .expect("the tree has room");
        assert_eq!(found.root(), computed.root(), "order {order:?}");
    }
}

/// Makes `updates` the updates of `parent` for the test transactions `expected`.
fn expect(updates: &mut TreeUpdates, parent: &Arc<NoteCommitmentTree>, expected: &[u8]) {
    updates.expect(
        parent,
        expected
            .iter()
            .map(|&transaction| (id(transaction), note_commitments(transaction))),
    );
}

/// The last three expected transactions have an update for every order of every one, two and
/// three of them, and each update is the one a block with those transactions makes.
#[test]
fn updates_are_for_every_order_of_the_last_three_expected_transactions() {
    let _init_guard = zebra_test::init();

    let parent = tree(3);
    let mut updates = TreeUpdates::default();

    expect(&mut updates, &parent, &[0]);
    check_updates(&updates, &parent, &[vec![0]]);

    expect(&mut updates, &parent, &[0, 1]);
    check_updates(
        &updates,
        &parent,
        &[vec![0], vec![1], vec![0, 1], vec![1, 0]],
    );

    expect(&mut updates, &parent, &[0, 1, 2]);
    check_updates(&updates, &parent, &orders_of([0, 1, 2]));

    expect(&mut updates, &parent, &[0, 1, 2, 3, 4]);
    check_updates(&updates, &parent, &orders_of([2, 3, 4]));
    assert_eq!(
        updates.transactions.len(),
        PRECOMPUTED_TREE_UPDATE_TRANSACTIONS
    );

    // A transaction that is named twice has its later place.
    expect(&mut updates, &parent, &[4, 5, 6, 7, 4]);
    check_updates(&updates, &parent, &orders_of([6, 7, 4]));

    expect(&mut updates, &parent, &[7]);
    check_updates(&updates, &parent, &[vec![7]]);

    expect(&mut updates, &parent, &[]);
    check_updates(&updates, &parent, &[]);
}

/// Expected transactions replace the ones before them, whatever they were and in whatever order
/// they were named, and the updates of transactions that stay are kept, not computed again.
#[test]
fn expected_transactions_replace_the_ones_before() {
    let _init_guard = zebra_test::init();

    let parent = tree(3);
    let mut updates = TreeUpdates::default();

    // Transactions 1 to 4, where a higher number is a likelier transaction. They were accepted
    // in the order 4, 2, 1, 3, each named with the likeliest transactions at that time.
    expect(&mut updates, &parent, &[4]);
    expect(&mut updates, &parent, &[2, 4]);
    expect(&mut updates, &parent, &[1, 2, 4]);
    expect(&mut updates, &parent, &[2, 3, 4]);
    check_updates(&updates, &parent, &orders_of([2, 3, 4]));

    let before = precomputed(&updates);

    // The same transactions again, as after a new tip that leaves the tree as it was, and in
    // another order.
    expect(&mut updates, &parent, &[2, 3, 4]);
    expect(&mut updates, &parent, &[4, 2, 3]);
    let after = precomputed(&updates);
    assert_eq!(before.len(), after.len());
    assert!(
        before
            .iter()
            .all(|update| after.iter().any(|kept| Arc::ptr_eq(update, kept))),
        "the updates of transactions that are expected again are kept",
    );

    // Transaction 4 was mined in a block that left this tree as it was, or it expired.
    expect(&mut updates, &parent, &[1, 2, 3]);
    check_updates(&updates, &parent, &orders_of([1, 2, 3]));
    let after = precomputed(&updates);
    let kept = before
        .iter()
        .filter(|update| after.iter().any(|kept| Arc::ptr_eq(update, kept)))
        .count();
    assert_eq!(kept, 4, "the updates of transactions 2 and 3 are kept");
}

/// The updates are for one tree: transactions that are expected on another tree drop the
/// updates of the tree before it. Another copy of the same tree is the same tree.
#[test]
fn updates_are_for_one_tree() {
    let _init_guard = zebra_test::init();

    let parent = tree(3);
    let mut updates = TreeUpdates::default();

    expect(&mut updates, &parent, &[0, 1]);
    let before = precomputed(&updates);
    expect(&mut updates, &Arc::new((*parent).clone()), &[0, 1]);
    let after = precomputed(&updates);
    assert!(before
        .iter()
        .all(|update| after.iter().any(|kept| Arc::ptr_eq(update, kept))));

    let next_parent = tree(4);
    expect(&mut updates, &next_parent, &[1, 2]);
    check_updates(
        &updates,
        &next_parent,
        &[vec![1], vec![2], vec![1, 2], vec![2, 1]],
    );
    let precomputed = precomputed(&updates);
    assert!(PrecomputedTreeUpdate::find(&precomputed, &parent, &note_commitments(1)).is_none());
}

/// A transaction without note commitments for the tree, or with more than the limit, has no
/// updates, and doesn't take the place of a transaction that has.
#[test]
fn transactions_outside_the_note_commitment_limits_have_no_updates() {
    let _init_guard = zebra_test::init();

    let parent = tree(3);
    let mut updates = TreeUpdates::default();
    let commitments = |count: usize| -> Vec<NoteCommitmentUpdate> {
        (0..count)
            .map(|action| NoteCommitmentUpdate::from(u64::try_from(action).expect("fits in u64")))
            .collect()
    };

    updates.expect(
        &parent,
        [
            (id(0), note_commitments(0)),
            (id(1), note_commitments(1)),
            (id(2), note_commitments(2)),
            (id(100), Vec::new()),
            (
                id(101),
                commitments(PRECOMPUTED_TREE_UPDATE_NOTE_COMMITMENTS + 1),
            ),
        ],
    );
    check_updates(&updates, &parent, &orders_of([0, 1, 2]));

    updates.expect(
        &parent,
        [(
            id(102),
            commitments(PRECOMPUTED_TREE_UPDATE_NOTE_COMMITMENTS),
        )],
    );
    assert_eq!(updates.updates.len(), 1);
    assert_eq!(updates.transactions.len(), 1);
}
