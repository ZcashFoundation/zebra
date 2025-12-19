# Ecosystem Integration Guide

This guide is for developers building applications that integrate with Zebra, including wallets, miners, block explorers, and other Zcash ecosystem tools.

## Overview

Zebra exposes functionality through:

1. **JSON-RPC API** - Compatible with zcashd RPC methods
2. **Metrics endpoint** - Prometheus-format metrics for monitoring
3. **Health endpoints** - For load balancers and orchestration

## JSON-RPC Integration

### Enabling the RPC Server

Add the following to your `zebrad.toml`:

```toml
[rpc]
listen_addr = "127.0.0.1:8232"

# For mining support
# mining_solver_enabled = true
```

### Available RPC Methods

Zebra implements a subset of zcashd-compatible RPC methods:

#### Blockchain Information

| Method              | Description                             |
| ------------------- | --------------------------------------- |
| `getblockchaininfo` | Chain state, network, and sync progress |
| `getblock`          | Get block by hash or height             |
| `getblockheader`    | Get block header only                   |
| `getblockhash`      | Get block hash by height                |
| `getblockcount`     | Current block height                    |
| `getbestblockhash`  | Tip block hash                          |

#### Transactions

| Method               | Description                        |
| -------------------- | ---------------------------------- |
| `getrawtransaction`  | Get raw transaction by txid        |
| `sendrawtransaction` | Submit a raw transaction           |
| `gettransaction`     | Get transaction with confirmations |

#### Network

| Method           | Description                  |
| ---------------- | ---------------------------- |
| `getinfo`        | General node information     |
| `getpeerinfo`    | Connected peer information   |
| `getnetworkinfo` | Network protocol information |

#### Mining

| Method             | Description                   |
| ------------------ | ----------------------------- |
| `getblocktemplate` | Get block template for mining |
| `submitblock`      | Submit a mined block          |
| `getmininginfo`    | Mining status                 |

### Example: Python Integration

```python
import requests
import json

def rpc_call(method, params=None):
    """Make a JSON-RPC call to Zebra."""
    payload = {
        "jsonrpc": "2.0",
        "id": "1",
        "method": method,
        "params": params or []
    }
    response = requests.post(
        "http://127.0.0.1:8232",
        json=payload,
        headers={"Content-Type": "application/json"}
    )
    result = response.json()
    if "error" in result and result["error"]:
        raise Exception(result["error"])
    return result["result"]

# Get blockchain info
info = rpc_call("getblockchaininfo")
print(f"Chain: {info['chain']}")
print(f"Blocks: {info['blocks']}")

# Get a specific block
block = rpc_call("getblock", ["1000000", 2])  # verbosity=2 for full tx data
print(f"Block hash: {block['hash']}")

# Send a raw transaction
# tx_hex = "..."  # Your signed transaction hex
# txid = rpc_call("sendrawtransaction", [tx_hex])
```

### Example: JavaScript/Node.js Integration

```javascript
const axios = require("axios");

async function rpcCall(method, params = []) {
  const response = await axios.post("http://127.0.0.1:8232", {
    jsonrpc: "2.0",
    id: "1",
    method,
    params,
  });

  if (response.data.error) {
    throw new Error(response.data.error.message);
  }
  return response.data.result;
}

// Example usage
async function main() {
  const info = await rpcCall("getblockchaininfo");
  console.log(`Chain: ${info.chain}`);
  console.log(`Blocks: ${info.blocks}`);

  const block = await rpcCall("getblock", ["1000000", 2]);
  console.log(`Block hash: ${block.hash}`);
}

main().catch(console.error);
```

### Example: Rust Integration

```rust
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i32,
    message: String,
}

#[derive(Debug, Deserialize)]
struct BlockchainInfo {
    chain: String,
    blocks: u64,
    // ... other fields
}

async fn rpc_call<T: for<'de> Deserialize<'de>>(
    method: &str,
    params: serde_json::Value,
) -> Result<T, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let response: RpcResponse<T> = client
        .post("http://127.0.0.1:8232")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": method,
            "params": params
        }))
        .send()
        .await?
        .json()
        .await?;

    match (response.result, response.error) {
        (Some(result), _) => Ok(result),
        (_, Some(error)) => Err(format!("RPC error: {}", error.message).into()),
        _ => Err("Empty response".into()),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let info: BlockchainInfo = rpc_call("getblockchaininfo", json!([])).await?;
    println!("Chain: {}", info.chain);
    println!("Blocks: {}", info.blocks);
    Ok(())
}
```

## Mining Integration

### Pool Setup with getblocktemplate

1. Enable mining in `zebrad.toml`:

   ```toml
   [rpc]
   listen_addr = "0.0.0.0:8232"

   [mining]
   miner_address = "t1YourTransparentAddress..."
   ```

2. Use the GBT (GetBlockTemplate) workflow:

   ```python
   # Get block template
   template = rpc_call("getblocktemplate", [{"capabilities": ["coinbasetxn"]}])

   # Mine the block (your mining software)
   # ...

   # Submit the mined block
   result = rpc_call("submitblock", [mined_block_hex])
   ```

See the [Mining Guide](mining.md) for detailed pool setup instructions.

## Lightwalletd Integration

Zebra is designed to work with [lightwalletd](https://github.com/zcash/lightwalletd) for light wallet support.

### Setup

1. Start Zebra with RPC enabled
2. Configure lightwalletd to connect to Zebra's RPC port
3. Light wallets connect to lightwalletd's gRPC interface

See the [Lightwalletd Guide](lightwalletd.md) for configuration details.

## Monitoring Integration

### Prometheus Metrics

Enable metrics in `zebrad.toml`:

```toml
[metrics]
endpoint_addr = "127.0.0.1:9999"
```

Available metrics include:

- Block height and sync progress
- Peer connections
- RPC request counts and latencies
- Memory and resource usage

See the [Metrics Guide](metrics.md) for Grafana dashboard setup.

### Health Endpoints

```toml
[tracing]
endpoint_addr = "127.0.0.1:3000"
```

Available endpoints:

- `GET /health` - Basic health check
- `GET /ready` - Readiness check (synced to tip)

See the [Health Endpoints Guide](health.md) for Kubernetes integration.

## Security Considerations

### Network Exposure

- **RPC**: By default, bind to `127.0.0.1` only. For remote access, use a reverse proxy with authentication.
- **Metrics**: Bind to internal network or use firewall rules.

### Rate Limiting

Consider implementing rate limiting in front of the RPC endpoint for public-facing services.

### Transaction Privacy

- Use shielded transactions when possible
- Consider Tor integration for privacy-sensitive applications

## Best Practices

1. **Connection Management**: Reuse HTTP connections for RPC calls
2. **Error Handling**: Always check for RPC errors in responses
3. **Sync State**: Check `getblockchaininfo` before relying on chain data
4. **Mempool**: Use `sendrawtransaction` responses to track submission status
5. **Block Confirmations**: Wait for sufficient confirmations before considering transactions final

## Related Resources

- [Zcash RPC Documentation](https://zcash.github.io/rpc/)
- [Lightwalletd Repository](https://github.com/zcash/lightwalletd)
- [Zaino (Zcash indexer)](https://github.com/zingolabs/zaino)
- [Zcash Protocol Specification](https://zips.z.cash/protocol/protocol.pdf)
