use halo2::pasta::{group::ff::PrimeField, pallas};
use sinsemilla::HashDomain;

use super::vectors;

// Check both specified domains over the vectors' message lengths and bit patterns.
#[test]
fn sinsemilla_hackworks_test_vectors() {
    for vector in vectors::SINSEMILLA.iter() {
        let domain = std::str::from_utf8(&vector.domain).expect("test-vector domains are UTF-8");
        let hash = HashDomain::new(domain).hash(vector.msg.iter().copied());

        assert_eq!(
            hash.expect("test-vector inputs satisfy Theorem 5.4.4"),
            pallas::Base::from_repr(vector.hash).unwrap()
        );
    }
}
