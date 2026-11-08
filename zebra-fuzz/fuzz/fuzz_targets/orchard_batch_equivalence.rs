//! Concatenated transactions plus one era/pool control byte; see equivalence.
#![no_main]
use libfuzzer_sys::fuzz_target;
use zebra_fuzz_targets::equivalence::orchard;

fuzz_target!(init: { let _ = orchard::keys(); }, |data: &[u8]| {
    orchard::check(data);
});
