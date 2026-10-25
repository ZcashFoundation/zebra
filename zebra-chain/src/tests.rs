//!Chain functionality for fetching test vectors.
//!

mod vectors;

#[test]
fn pasta_asm_requires_both_cpu_flags() {
    use crate::diagnostic::pasta_asm_supported;

    assert!(pasta_asm_supported(true, true));
    assert!(!pasta_asm_supported(false, true));
    assert!(!pasta_asm_supported(true, false));
    assert!(!pasta_asm_supported(false, false));
}
