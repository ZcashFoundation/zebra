# Developer Onboarding Guide

Welcome to Zebra development! This guide will help you get your development environment set up and make your first contribution.

## Prerequisites

Before you begin, ensure you have the following installed:

### Required

- **Rust** (1.89 or later)

  ```bash
  # Install via rustup
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

  # Verify installation
  rustc --version
  cargo --version
  ```

- **Git**

  ```bash
  git --version
  ```

### Platform-Specific Dependencies

#### Linux (Debian/Ubuntu)

```bash
sudo apt-get update
sudo apt-get install -y \
    build-essential \
    clang \
    libclang-dev \
    protobuf-compiler
```

#### macOS

```bash
# Install Xcode command line tools
xcode-select --install

# Install protobuf via Homebrew
brew install protobuf
```

#### Windows

We recommend using WSL2 with Ubuntu. See the [official WSL installation guide](https://docs.microsoft.com/en-us/windows/wsl/install).

## Clone and Build

```bash
# Clone the repository
git clone https://github.com/ZcashFoundation/zebra.git
cd zebra

# Build in release mode (recommended for testing)
cargo build --release

# Or build in debug mode (faster compilation, slower runtime)
cargo build
```

**Note**: The first build will take several minutes as it compiles all dependencies.

## Running Zebra

```bash
# Run with default configuration (Mainnet)
cargo run --release

# Run on Testnet
cargo run --release -- --network testnet

# Run with custom config file
cargo run --release -- --config /path/to/zebrad.toml
```

### Configuration

Create a `zebrad.toml` configuration file:

```toml
[network]
network = "Mainnet"
listen_addr = "0.0.0.0:8233"

[state]
cache_dir = "/path/to/zebra/state"

[tracing]
filter = "info"
```

See the [user documentation](../user/run.md) for all configuration options.

## Running Tests

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p zebra-chain

# Run a specific test
cargo test transaction_roundtrip

# Run tests with output visible
cargo test -- --nocapture

# Run only unit tests (faster)
cargo test --lib

# Run only integration tests
cargo test --test '*'
```

### Test Categories

| Command                         | What it Tests         |
| ------------------------------- | --------------------- |
| `cargo test -p zebra-chain`     | Core data structures  |
| `cargo test -p zebra-consensus` | Verification logic    |
| `cargo test -p zebra-state`     | State storage         |
| `cargo test -p zebra-network`   | P2P networking        |
| `cargo test -p zebrad`          | Full node integration |

## Understanding the Codebase

### Crate Structure

Zebra is organized into several crates, each with a specific responsibility:

```text
zebra/
├── zebrad/              # Main application binary
├── zebra-chain/         # Core data structures (Block, Transaction, etc.)
├── zebra-consensus/     # Verification (signatures, proofs, scripts)
├── zebra-state/         # State storage (RocksDB)
├── zebra-network/       # P2P networking
├── zebra-rpc/           # JSON-RPC server
├── zebra-node-services/ # Service trait definitions
├── zebra-script/        # Script verification
├── zebra-test/          # Test utilities
└── zebra-utils/         # CLI tools
```

See the [Crate Architecture Diagram](diagrams/crate-architecture.md) for visual dependency relationships.

### Key Concepts

#### Tower Services

Zebra uses the [Tower](https://docs.rs/tower/) framework for async services. All major components implement `tower::Service`:

```rust
impl Service<Request> for MyService {
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // Handle request
    }
}
```

#### Three-Level Verification

Zebra separates validation into three stages:

1. **Structural** - Enforced by the type system (invalid states are unrepresentable)
2. **Semantic** - Cryptographic verification (signatures, proofs) in `zebra-consensus`
3. **Contextual** - Chain state checks (UTXOs, nullifiers) in `zebra-state`

#### Request/Response Enums

Internal communication uses typed enums:

```rust
// In zebra-state
pub enum Request {
    Block(block::Hash),
    Transaction(transaction::Hash),
    // ...
}

pub enum Response {
    Block(Option<Arc<Block>>),
    Transaction(Option<Arc<Transaction>>),
    // ...
}
```

## Your First Contribution

### 1. Find an Issue

Look for issues labeled:

- [`good first issue`](https://github.com/ZcashFoundation/zebra/labels/good%20first%20issue) - Beginner-friendly
- [`E-help-wanted`](https://github.com/ZcashFoundation/zebra/labels/E-help-wanted) - Help needed

### 2. Create a Branch

```bash
git checkout -b your-feature-name
```

### 3. Make Your Changes

- Follow the [coding standards](../CONTRIBUTING.md#coding-standards)
- Add tests for new functionality
- Update documentation if needed

### 4. Run Checks Locally

```bash
# Format code
cargo fmt

# Run clippy lints
cargo clippy --all-targets --all-features

# Run tests
cargo test

# Build documentation
cargo doc --no-deps
```

### 5. Commit and Push

```bash
# Stage changes
git add .

# Commit with conventional commit message
git commit -m "feat(chain): add new transaction validation"

# Push to your fork
git push origin your-feature-name
```

### 6. Create a Pull Request

- Use a [conventional commits](https://www.conventionalcommits.org/) title
- Fill in the PR template
- Wait for CI to pass
- Address review feedback

## Development Tools

### Useful Commands

```bash
# Check for unused dependencies
cargo +nightly udeps

# Generate documentation
cargo doc --open

# Run specific binary
cargo run --bin zebra-checkpoints

# Check all feature combinations
cargo hack check --feature-powerset

# Profile with flamegraph
cargo flamegraph --bin zebrad
```

### IDE Setup

#### VS Code

Install the [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer) extension.

Recommended settings (`.vscode/settings.json`):

```json
{
  "rust-analyzer.check.command": "clippy",
  "rust-analyzer.cargo.features": "all"
}
```

#### IntelliJ/CLion

Install the [Rust plugin](https://plugins.jetbrains.com/plugin/8182-rust).

### Debugging

#### Using tracing

Zebra uses the `tracing` crate for structured logging:

```bash
# Run with debug output
RUST_LOG=debug cargo run --release

# Filter to specific modules
RUST_LOG=zebra_network=trace,zebra_state=debug cargo run --release
```

#### Using tokio-console

For async debugging, see the [tokio-console guide](tokio-console.md).

## Troubleshooting

### Build Issues

#### Out of Memory During Build

```bash
# Limit parallel jobs
export CARGO_BUILD_JOBS=2
cargo build --release
```

#### Missing Dependencies

If you see errors about missing headers or libraries:

```bash
# Linux: Install build essentials
sudo apt-get install build-essential clang libclang-dev

# macOS: Install Xcode tools
xcode-select --install
```

#### Protobuf Errors

```bash
# Linux
sudo apt-get install protobuf-compiler

# macOS
brew install protobuf
```

### Test Issues

#### Tests Timing Out

```bash
# Limit test parallelism
cargo test -- --test-threads=2

# Skip network-dependent tests
SKIP_NETWORK_TESTS=1 cargo test
```

#### Property Tests Failing

If proptest tests fail randomly, increase the test cases:

```bash
PROPTEST_CASES=1000 cargo test
```

### CI Failures

If CI fails on your PR:

1. **Check the failed job** - Click on the failing check to see logs
2. **Common causes**:
   - `cargo fmt` - Run `cargo fmt` locally
   - `cargo clippy` - Run `cargo clippy --all-targets --all-features`
   - Missing docs - Add `///` documentation to public items
   - Feature combinations - Run `cargo hack check --feature-powerset`

### Performance Profiling

#### Using cargo-flamegraph

```bash
# Install
cargo install flamegraph

# Generate flamegraph
cargo flamegraph --bin zebrad -- --config zebrad.toml
```

#### Using perf (Linux)

```bash
# Record
perf record --call-graph dwarf target/release/zebrad

# Report
perf report
```

## Getting Help

- **Discord**: [Join the Zcash Foundation Discord](https://discord.gg/yVNhQwQE68)
- **GitHub Issues**: [Open an issue](https://github.com/ZcashFoundation/zebra/issues)
- **Documentation**: [zebra.zfnd.org](https://zebra.zfnd.org)

## Next Steps

Once you're comfortable with the basics:

- Read the [Design Overview](overview.md) for architecture details
- Explore the [RFCs](rfcs.md) for design decisions
- Check the [API documentation](https://docs.rs/zebrad) on docs.rs
