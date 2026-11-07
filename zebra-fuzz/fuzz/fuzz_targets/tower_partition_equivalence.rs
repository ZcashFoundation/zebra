//! Concatenated transactions plus one partition control byte; see equivalence.
#![no_main]
use libfuzzer_sys::fuzz_target;
use zebra_fuzz_targets::equivalence::tower;

fuzz_target!(init: { let _ = tower::runtime(); }, |data: &[u8]| {
    tower::check(data);
});
