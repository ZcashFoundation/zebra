# Zebra + S-NOMP Mining Pool

Docker Compose setup for running a Zcash mining pool with Zebra and S-NOMP.

## Architecture

```
┌─────────────┐       ┌───────────┐      ┌─────────────┐
│   Miners    │─────▶│  S-NOMP   │────▶│    Zebra    │
│ (nheqminer, │:3333  │ (Stratum) │:RPC  │ (Full Node) │
│  Antminer)  │       │           │      │             │
└─────────────┘       └──────┬────┘      └─────────────┘
                             │
                            ▼
                       ┌─────────┐
                       │  Redis  │
                       │ (Stats) │
                       └─────────┘
```

- **Zebra**: Zcash full node - validates blocks, provides `getblocktemplate`
- **S-NOMP**: Stratum mining pool - distributes work to miners, submits blocks
- **Redis**: Stores share counts and pool statistics

All block rewards go to the address configured in `MINER_ADDRESS`.

## Quick Start

```bash
# 1. Configure
cp .env.example .env
# Edit .env - set MINER_ADDRESS (required)

# 2. Start
docker compose up -d

# 3. Wait for Zebra to sync (hours)
docker compose logs -f zebra

# 4. Once synced, start mining (choose one):

# Option A: Built-in nheqminer container
docker compose --profile miner up -d nheqminer

# Option B: External miner
nheqminer -l <host>:3333 -u <address>.worker1 -t <threads>
```

## Configuration

Edit `.env` to configure:

| Variable        | Default    | Description                                  |
|-----------------|------------|----------------------------------------------|
| `MINER_ADDRESS` | (required) | Transparent address for block rewards        |
| `NETWORK`       | `Testnet`  | `Mainnet` or `Testnet`                       |
| `RPC_PORT`      | `18232`    | Zebra RPC port (18232 Testnet, 8232 Mainnet) |
| `PEER_PORT`     | `18233`    | P2P port (18233 Testnet, 8233 Mainnet)       |
| `STRATUM_PORT`  | `3333`     | Port miners connect to                       |
| `WORKER_NAME`   | `docker`   | Worker name suffix (for nheqminer container) |
| `CPU_THREADS`   | `1`        | CPU threads for nheqminer                    |

## Checking Sync Status

Zebra must fully sync before mining can begin.

```bash
# Quick status
docker exec zebra curl -s -H "Content-Type: application/json" \
  localhost:18232 -d '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}' \
  | grep -oE '"(blocks|estimatedheight)":[0-9]+'

# Watch sync progress
docker logs -f zebra 2>&1 | grep -E "state_tip|verified"

# Detailed info
docker exec zebra curl -s -H "Content-Type: application/json" \
  localhost:18232 -d '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}'
```

## Connecting Miners

### Built-in nheqminer Container

The setup includes an optional nheqminer container for CPU mining. It uses a Docker Compose profile, so it won't start by default.

```bash
# Start the miner (after Zebra is synced)
docker compose --profile miner up -d nheqminer

# View miner logs
docker compose logs -f nheqminer

# Stop the miner
docker compose --profile miner stop nheqminer

# Adjust CPU threads in .env
CPU_THREADS=4
```

### External nheqminer (CPU/GPU)

```bash
# CPU mining
./nheqminer -l <pool-host>:3333 -u <your-address>.worker1 -t <threads>

# Example
./nheqminer -l 192.168.1.100:3333 -u t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v.rig1 -t 4
```

### Antminer Z15

1. Open miner web interface
2. Go to **Miner Configuration**
3. Set:
   - **Pool URL**: `stratum+tcp://<pool-host>:3333`
   - **Worker**: `<your-address>.z15`
   - **Password**: `x`

## Web Interface

S-NOMP provides a web UI at `http://<host>:8080` showing:
- Pool hashrate
- Connected workers
- Found blocks
- Per-worker statistics

API endpoint: `http://<host>:8080/api/stats`

## Ports

| Port       | Service | Purpose                       |
|------------|---------|-------------------------------|
| 3333       | S-NOMP  | Stratum (miners connect here) |
| 8080       | S-NOMP  | Web UI and API                |
| 18233/8233 | Zebra   | P2P network (Testnet/Mainnet) |

Internal only (not exposed):
- 18232/8232: Zebra RPC
- 6379: Redis

## Operations

```bash
# Start all services (without miner)
docker compose up -d

# Start all services including miner
docker compose --profile miner up -d

# Stop all services
docker compose down

# View logs
docker compose logs -f           # All services
docker compose logs -f zebra     # Zebra only
docker compose logs -f s-nomp    # S-NOMP only
docker compose logs -f nheqminer # Miner only

# Restart a service
docker compose restart s-nomp

# Rebuild after updates
docker compose build --no-cache s-nomp
docker compose build --no-cache nheqminer
docker compose up -d s-nomp

# Check service status
docker compose ps
docker compose --profile miner ps  # Include miner

# Shell into container
docker compose exec zebra bash
docker compose exec s-nomp bash
docker compose exec nheqminer bash
```

## Data Persistence

Zebra chain data is stored in a Docker volume (`zebra-data`). This persists across container restarts.

```bash
# View volume
docker volume ls | grep mining

# Remove all data (will require full resync!)
docker compose down -v
```

## Troubleshooting

### S-NOMP: "mempool is not active"

Zebra is still syncing. Wait for sync to complete.

```bash
docker compose logs zebra | tail -20
```

### S-NOMP keeps restarting

Check logs for errors:

```bash
docker compose logs s-nomp | tail -50
```

### Miners can't connect

1. Check S-NOMP is running: `docker compose ps`
2. Check port is open: `nc -zv <host> 3333`
3. Check firewall allows port 3333

### Zebra not syncing

1. Check peer connections:
   ```bash
   docker exec zebra curl -s -H "Content-Type: application/json" \
     localhost:18232 -d '{"jsonrpc":"2.0","id":1,"method":"getinfo","params":[]}' \
     | grep connections
   ```
2. Ensure port 18233 (Testnet) or 8233 (Mainnet) is accessible

### Reset everything

```bash
docker compose down -v
docker compose up -d
```

## Switching Networks

To switch between Testnet and Mainnet:

1. Stop services: `docker compose down`
2. Edit `.env`:
   ```bash
   NETWORK=Mainnet
   MINER_ADDRESS=t1YourMainnetAddress
   ```
3. Start: `docker compose up -d`

## Security Notes

- RPC port is internal only (not exposed to host)
- Cookie authentication is disabled for S-NOMP compatibility
