---
status: accepted
date: 2025-06-18
story: Implement figment for Zebra config to simplify Docker configuration and enable better environment variable support
---

# Implement Figment Configuration System for Zebra

## Context and Problem Statement

Zebra currently uses Abscissa's configuration system with manual Docker entrypoint scripts that dynamically generate configuration files from environment variables. This approach has several issues: the Docker entrypoint script (`zebra/docker/entrypoint.sh`) contains complex bash logic that essentially reimplements configuration layering functionality, making it difficult to maintain and extend. Environment variable support is limited and requires manual mapping in the bash script. The configuration system lacks the flexibility and type safety that modern configuration libraries provide.

Zaino and Zallet have successfully implemented figment for their configuration systems, demonstrating a clear path forward and consistent approach across the ZF ecosystem.

## Priorities & Constraints

* Maintain backward compatibility with existing zebrad.toml configuration files
* Preserve current command-line argument functionality  
* Simplify Docker container configuration without breaking existing deployments
* Ensure type safety and validation for configuration values
* Reduce maintenance burden of the Docker entrypoint script
* Align with configuration patterns already established in Zaino and Zallet

## Considered Options

* Option 1: Keep current Abscissa + bash script approach
* Option 2: Implement figment configuration system
* Option 3: Create custom configuration system from scratch

### Pros and Cons of the Options

#### Option 1: Keep current Abscissa + bash script approach

* Good, because it requires no changes to existing code
* Good, because it maintains complete backward compatibility
* Bad, because the bash script is complex and difficult to maintain
* Bad, because adding new environment variables requires bash script modifications
* Bad, because it doesn't provide type safety for environment variable conversion
* Bad, because it's inconsistent with Zaino and Zallet approaches

#### Option 2: Implement figment configuration system

* Good, because it provides layered configuration (Defaults → TOML → Environment Variables)
* Good, because it handles type conversions and validation automatically
* Good, because it supports nested environment variables with standard naming conventions
* Good, because it's already proven in Zaino implementation
* Good, because it reduces Docker entrypoint script complexity significantly
* Good, because it provides better error messages for configuration issues
* Good, because it makes adding new configuration options easier
* Bad, because it requires changes to the current Abscissa integration
* Bad, because it adds a new dependency (figment)

#### Option 3: Create custom configuration system from scratch

* Good, because it could be tailored specifically to Zebra's needs
* Bad, because it would require significant development and testing effort
* Bad, because it would create a third different configuration approach in the ZF ecosystem
* Bad, because it would lack the battle-testing that figment has received

## Decision Outcome

Chosen option: **Option 2: Implement figment configuration system**

This option provides the best balance of functionality, maintainability, and ecosystem consistency. The benefits of simplified configuration management, better type safety, and reduced Docker complexity outweigh the costs of integration changes. The successful implementation in Zaino and Zallet provides a proven path forward and reduces implementation risk.

### Expected Consequences

* Docker entrypoint script will be significantly simplified
* Environment variable configuration will be more robust and easier to extend
* Configuration error messages will be more helpful to users
* New configuration options can be added more easily
* Consistency across ZF ecosystem will be improved
* Some integration work will be required to replace Abscissa configuration loading

## More Information

* Figment documentation: https://docs.rs/figment/latest/figment/
* Current environment variables supported: NETWORK, ZEBRA_CACHE_DIR, ZEBRA_COOKIE_DIR, ZEBRA_RPC_PORT, RPC_LISTEN_ADDR, ENABLE_COOKIE_AUTH, METRICS_ENDPOINT_ADDR, METRICS_ENDPOINT_PORT, LOG_FILE, LOG_COLOR, USE_JOURNALD, TRACING_ENDPOINT_ADDR, TRACING_ENDPOINT_PORT, MINER_ADDRESS, FEATURES
