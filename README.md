# ProjectDawn Eclipse L2

Eclipse-based SVM L2 chain with custom DAWN tokenomics.

## Overview

ProjectDawn is an L2 blockchain built on [Eclipse's Agave fork](https://github.com/Eclipse-Laboratories-Inc/agave) (3341 commits ahead of upstream [anza-xyz/agave](https://github.com/anza-xyz/agave)). It operates as a single-sequencer L2 using Solana's validator terminology for compatibility, replacing SOL with a custom **DAWN** token (symbol: ☀).

All ProjectDawn tokenomics are **feature-gated** -- disabled by default and activated on-chain via a deterministic feature pubkey.

## Key Features

- **Permanent Stake Locking** -- Lock stakes permanently in exchange for a 120% reward bonus (20% extra on top of base rewards).
- **Passive Staking** -- 5-tier lockup system (30/90/180/365/730 days) with proportional reward multipliers.
- **4-Way Fee Split** -- EIP-1559-style fee distribution between burn, validator, treasury, and developer accounts.
- **Flat 5% APY** -- Replaces Solana's declining inflation curve with a fixed annual staking yield.
- **Developer Fee Attribution** -- Programs earn a share of transaction fees proportional to their usage.
- **On-Chain Treasury** -- Seeded at genesis, receives its share of every fee split.
- **Feature Gate** -- All ProjectDawn economics are gated behind on-chain feature activation, allowing the chain to launch with stock Solana economics and transition when ready.

## Pre-built Binaries

Pre-built binaries for **Linux x86-64** are available on the [Releases](../../releases) page. All other platforms must build from source (see below).

## Building from Source

### Prerequisites

- Rust 1.84.1 or later
- Standard Solana/Agave build dependencies (OpenSSL, pkg-config, protobuf, clang, etc.)

On Ubuntu/Debian:
```bash
sudo apt-get update
sudo apt-get install libssl-dev libudev-dev pkg-config zlib1g-dev llvm clang cmake make libprotobuf-dev protobuf-compiler libclang-dev
```

### Build

```bash
# May be needed on some Linux systems:
export PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig

cargo build --release --bin agave-validator
cargo build --release --bin solana
cargo build --release --bin solana-genesis
cargo build --release --bin solana-keygen
```

## Launching a Testnet

```bash
# 1. Create genesis with ProjectDawn treasury
./scripts/projectdawn-testnet.sh setup

# 2. Start bootstrap validator (acts as sequencer in L2 mode)
multinode-demo/bootstrap-validator.sh

# 3. (Optional) Start additional validators
multinode-demo/validator.sh

# 4. Activate ProjectDawn feature
./scripts/projectdawn-testnet.sh activate

# 5. Check status
./scripts/projectdawn-testnet.sh status
```

> **Note:** Feature activation requires a keypair matching pubkey `FfysvyBPqGve3oDPu14LB1UqR8B2v7CeJ6EajWdx8P8D` placed at `config/projectdawn-feature.json`.

## Running Tests

```bash
# Core test suites (~723 passing)
cargo test -p solana-runtime
cargo test -p solana-stake-program
cargo test -p solana-stake-interface

# ProjectDawn integration tests
cargo test -p solana-runtime --test projectdawn_permanent
cargo test -p solana-runtime --test projectdawn_passive
cargo test -p solana-runtime --test projectdawn_fees
```

## Project Structure

ProjectDawn-specific files and directories:

```
solana-stake-interface/                        # Custom stake instructions (PermanentLock, PassiveLock, EarlyUnlock)
solana-native-token/                           # DAWN token branding (☀ symbol)
runtime/src/projectdawn_config.rs              # Configuration (APY, fee splits, transition)
runtime/tests/projectdawn_permanent.rs         # Permanent lock integration tests (11 tests)
runtime/tests/projectdawn_passive.rs           # Passive staking integration tests (4 tests)
runtime/tests/projectdawn_fees.rs              # Fee split integration tests (5 tests)
scripts/projectdawn-testnet.sh                 # Testnet launch script
programs/stake/src/stake_state.rs              # Modified stake program with permanent lock guards
genesis/src/main.rs                            # Treasury seeding at genesis
```

## Configuration

Default ProjectDawn configuration (defined in `projectdawn_config.rs`):

| Parameter | Value |
|-----------|-------|
| Annual staking APY | 5% |
| Fee transition period | 10 years (1460 epochs) |
| Launch fee split | 10% burn / 0% validator / 45% treasury / 45% developer |
| Maturity fee split | 25% burn / 25% validator / 25% treasury / 25% developer |
| Feature gate pubkey | `FfysvyBPqGve3oDPu14LB1UqR8B2v7CeJ6EajWdx8P8D` |

## Known Limitations

- **GovernanceUnlock** is a stub (returns error) -- governance-based unlocking is not yet implemented.
- **No multi-validator consensus testing** -- the L2 operates as a single-sequencer; multi-validator topologies are untested.
- **No fuzz testing** -- property-based and fuzz testing has not been performed.
- **Feature keypair** -- The private key for the deterministic feature gate pubkey must be provided externally.

## License

Same as upstream Agave -- [Apache 2.0](LICENSE).
