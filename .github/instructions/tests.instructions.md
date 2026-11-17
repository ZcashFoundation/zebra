---
applyTo: "**/tests/**,**/tests.rs,**/*_tests.rs,**/prop.rs,**/vectors.rs,zebra-test/**"
excludeAgent: "cloud-agent"
---

# Test review

- For changed limit tests, check that the assertions distinguish the limit from the first value past it. For changed consensus fixtures, verify that the expected bytes or results come from the governing specification, an independent implementation, or published test vectors.
- Check that a new regression test fails with the old behavior. A successful round trip alone cannot establish wire compatibility.
