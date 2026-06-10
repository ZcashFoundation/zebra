//! Tests for canonical Orchard proof sizes.

use crate::orchard::shielded_data::expected_proof_size;

/// The canonical Orchard proof size for `n` actions is `2272·n + 2720` bytes, matching
/// the Orchard circuit's `halo2_proofs` `CircuitCost` (4992 bytes for 1 action, 7264 for
/// 2 actions). These values are consensus-critical, so pin them here.
#[test]
fn expected_proof_size_known_values() {
    assert_eq!(expected_proof_size(0), 2720);
    assert_eq!(expected_proof_size(1), 4992);
    assert_eq!(expected_proof_size(2), 7264);
    assert_eq!(expected_proof_size(3), 9536);
}
