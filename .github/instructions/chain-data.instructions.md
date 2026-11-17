---
applyTo: "zebra-chain/**"
excludeAgent: "cloud-agent"
---

# Chain data and serialization review

- Compare changed consensus encodings with the governing protocol specification or ZIP. Check field order, separately encoded arrays, and fields present only in particular versions.
- Check the bytes passed to transaction digests and sighashes. A derived encoding or a round-trip test can agree with itself while encoding the wrong bytes.
