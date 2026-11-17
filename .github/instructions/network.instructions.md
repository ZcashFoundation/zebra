---
applyTo: "zebra-network/**,zebra-node-services/**,zebrad/src/components/sync/**,zebrad/src/components/mempool/**,zebrad/src/components/inbound/**,zebrad/src/components/sync.rs,zebrad/src/components/mempool.rs,zebrad/src/components/inbound.rs"
excludeAgent: "cloud-agent"
---

# Network, peer, mempool, and sync review

- Check resource limits across connection types. Count background connection attempts against the connection budget and check that one peer cannot consume a shared deadline.
- Trace peer attribution through the validation result. Keep the source peer and misbehavior score when wrapping errors, and credit responses after validation.
- Compare changed wire fields, protocol versions, and messages with the applicable specification, ZIP, or zcashd behavior. Preserve supported older-peer behavior. The version decoder deliberately ignores unknown service bits. Flag their acceptance only where the relevant protocol field requires rejection.
