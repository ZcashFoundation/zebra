//! Orchard/Ironwood bundles: seeded aggregate vs direct proof and signature
//! verification. Bundle extraction is harness code; both verification paths
//! are the backend crate's public APIs. A finding needs independent triage.

use std::sync::OnceLock;

use orchard::{
    bundle::{Authorized, BatchValidator, Bundle},
    circuit::{OrchardCircuitVersion, VerifyingKey},
};
use rand::seq::SliceRandom;
use zcash_protocol::value::ZatBalance;

use super::{assert_no_false_accept, rng, seed, sighash, transactions, MAX_ITEMS};

pub struct Item {
    pub bundle: Bundle<Authorized, ZatBalance>,
    pub sighash: [u8; 32],
    pub ironwood: bool,
}

pub fn keys() -> &'static [VerifyingKey; 3] {
    static KEYS: OnceLock<[VerifyingKey; 3]> = OnceLock::new();
    KEYS.get_or_init(|| {
        [
            OrchardCircuitVersion::InsecurePreNu6_2,
            OrchardCircuitVersion::FixedPostNu6_2,
            OrchardCircuitVersion::PostNu6_3,
        ]
        .map(VerifyingKey::build)
    })
}

pub fn items(stream: &[u8]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut actions = 0;
    for tx in transactions(stream) {
        let Some(hash) = sighash(&tx) else { continue };
        for (bundle, ironwood) in [(tx.orchard_bundle(), false), (tx.ironwood_bundle(), true)] {
            if let Some(bundle) = bundle {
                // Bound total proof work, not just the number of bundles.
                if items.len() == MAX_ITEMS || actions + bundle.actions().len() > 64 {
                    return items;
                }
                actions += bundle.actions().len();
                items.push(Item {
                    bundle: bundle.clone(),
                    sighash: hash.0,
                    ironwood,
                });
            }
        }
    }
    items
}

pub fn batch(items: &[&Item], key: &VerifyingKey, seed: u64) -> Vec<bool> {
    let mut verifier = BatchValidator::new(key);
    let queued: Vec<_> = items
        .iter()
        .map(|item| verifier.add_bundle(&item.bundle, item.sighash).is_ok())
        .collect();
    let shared = verifier.validate(rng(seed));
    queued.into_iter().map(|ok| ok && shared).collect()
}

pub fn single(item: &Item, key: &VerifyingKey) -> bool {
    // add_bundle also enforces the cross-address restriction. Preserve that
    // admission condition before taking the independent verification path.
    if BatchValidator::new(key)
        .add_bundle(&item.bundle, item.sighash)
        .is_err()
    {
        return false;
    }
    let signatures = item.bundle.actions().iter().all(|action| {
        action
            .rk()
            .create_batch_item(action.authorization().clone(), &item.sighash)
            .verify_single()
            .is_ok()
    }) && item
        .bundle
        .binding_validating_key()
        .create_batch_item(
            item.bundle.authorization().binding_signature().clone(),
            &item.sighash,
        )
        .verify_single()
        .is_ok();
    signatures && item.bundle.verify_proof(key).is_ok()
}

/// Returns the number of selected bundles, for corpus reachability checks.
pub fn check(data: &[u8]) -> usize {
    let Some((&control, stream)) = data.split_last() else {
        return 0;
    };
    let owned = items(stream);
    let mut items: Vec<_> = owned
        .iter()
        .filter(|item| match control >> 6 {
            1 => !item.ironwood,
            2 => item.ironwood,
            _ => true,
        })
        .collect();
    if items.is_empty() {
        return 0;
    }
    let era = usize::from(control) % 3;
    let key = &keys()[era];
    let seed = seed(data);
    let singles: Vec<_> = items.iter().map(|item| single(item, key)).collect();
    let original = batch(&items, key, seed);
    assert_no_false_accept(&original, &singles);

    items.shuffle(&mut rng(seed ^ 0x00d1_00d1_00d1_00d1));
    let shuffled_singles: Vec<_> = items.iter().map(|item| single(item, key)).collect();
    assert_no_false_accept(&batch(&items, key, seed), &shuffled_singles);
    let mut doubled = items.clone();
    doubled.extend_from_slice(&items);
    let mut doubled_singles = shuffled_singles.clone();
    doubled_singles.extend_from_slice(&shuffled_singles);
    assert_no_false_accept(&batch(&doubled, key, seed), &doubled_singles);

    // The control byte is only an era claim. Check wrong-era rejection only
    // after both paths accept that claim, avoiding false alarms on relabelling.
    if singles.iter().all(|&ok| ok) && original.iter().all(|&ok| ok) {
        for (other, key) in keys().iter().enumerate() {
            if other != era {
                assert!(
                    !batch(&items, key, seed).into_iter().all(|ok| ok),
                    "wrong era batch acceptance"
                );
                assert!(
                    !items.iter().all(|item| single(item, key)),
                    "wrong era single acceptance"
                );
            }
        }
    }
    items.len()
}
