---
applyTo: "README.md,SECURITY.md,book/**/*.md,docs/**/*.md,docker/**/*.md,docker/**/*.toml,docker/**/*.conf,docker/**/.env,docker/**/.env.example,docker/**/docker-compose*.yml,docker/**/docker-compose*.yaml"
excludeAgent: "cloud-agent"
---

# Setup and configuration review

- Flag defaults that expose JSON-RPC, gRPC, metrics, tracing, or health endpoints to public networks. For intentional opt-in exposure, check that the example explains the required access controls and transport protection. A container-internal bind can be necessary for communication between services, so check the published host ports before flagging it.
- Check security claims against the protections the code and configuration actually provide. A guide for serving remote light wallets should show where TLS terminates and keep Zebra's plaintext gRPC backend on a loopback or private address.
