# Project Overview

**Branch:** `sniper` @ `e0cad975` · **Generated:** 2026-07-29 · **Repository type:** dual monorepo

## Purpose

A fork of [`sevenlabs-hq/carbon`](https://github.com/sevenlabs-hq/carbon), a Solana indexing
framework, carrying one product built on it: a **pump.fun creator-wallet sniper**.

The fork's own contribution is `examples/pumpfun-sniper/` (21 files, ~5,100 lines) plus two root
documents (`SNIPER_PLAN.md`, `SNIPER_HANDOFF.md`). Everything else is upstream Carbon.

## What the product does

Watch a set of creator wallets over Yellowstone gRPC. The instant one launches a pump.fun coin, fire
30 independent buy transactions from 30 wallets, concurrently, across up to four send routes — racing
to land in the create's block or the next one.

It handles both launch flows: classic `create` (SPL Token, native SOL) and `create_v2` (Token-2022
trading against a WSOL quote mint, ~21% of sampled live traffic).

**It cannot sell.** No position tracking, no exit. Phase 2, unbuilt.

## Tech Stack

| Category | Technology | Version |
|---|---|---|
| Language | Rust | **1.88.0** pinned (`rust-toolchain.toml`); MSRV 1.82 |
| Framework | `carbon-core` | **1.0.0** |
| Solana crates | `solana-client`, `solana-pubkey`, `solana-transaction`, … | 3.1.5 |
| Async | Tokio (full), `futures` | — |
| Data source | `carbon-yellowstone-grpc-datasource` + `yellowstone-grpc-proto` | 10.x |
| Decoder | `carbon-pumpfun-decoder` | 1.0.0 |
| Serialization | Borsh | workspace |
| TLS | `rustls` + `aws-lc-rs` | required provider install at startup |
| HTTP | `reqwest` | fast providers, Jito |
| Metrics | `carbon-log-metrics` | 1.0.0 |
| Codegen CLI | TypeScript — `packages/{cli,renderer,versions}` | pnpm + turbo |

## Repository Structure

```
carbon/
├── crates/core/              # carbon-core 1.0.0 — the framework
├── decoders/                 # 63 generated per-program decoder crates
├── datasources/              # 14 update producers
├── metrics/                  # log + prometheus exporters
├── examples/
│   ├── pumpfun-sniper/       # ◄── THE PRODUCT
│   └── (10 upstream examples)
├── packages/                 # TS codegen CLI (pnpm/turbo)
├── scripts/                  # cargo fmt/clippy, publish helpers
├── SNIPER_PLAN.md            # architecture rationale
├── SNIPER_HANDOFF.md         # resume + deployment guide
└── docs/                     # ◄── this documentation set
```

Gone relative to the old `test/1` architecture: `swap/` (TypeScript service), `ws/` (probe binary),
`clean.js`, `wallets.json`, and `crates/cli` (now `packages/`).

## Architecture at a Glance

```
Yellowstone gRPC ──> carbon Pipeline ──> SniperProcessor ──> mpsc(1024) ──> BuyDispatcher
   (Processed)         (PumpfunDecoder)     9 guards          try_send      30 txs parallel
                                                                                 │
                                            ┌────────────────┬─────────┬─────────┴────┐
                                          rpc              fast      jito           tpu
                                        (spray all)     (per-wallet) (6 bundles/  (QUIC to
                                                                      6 regions)   leaders)
```

Two design commitments drive most of the code:

1. **Zero RPC on the hot path.** Every PDA is derived locally; blockhash, TPU connections, and Jito
   tip floor are kept warm by background tasks. Between seeing a create and sending, the only network
   I/O is the send.
2. **Redundant delivery is free.** A signature lands at most once, so the same transaction is sprayed
   down every enabled route; the fastest wins and the rest are no-ops. A tip only costs anything if
   that transaction is the one that lands.

## Current Status

| | |
|---|---|
| Builds | ✅ clean, 3m45s |
| Tests | ✅ **36/36 passing** (verified 2026-07-29) |
| Buy account layout | ✅ verified against real mainnet buys, v1 and v2 |
| Live snipe | ❌ **never executed — not one transaction** |
| Simulate vs live geyser | ❌ never run |
| Block 0 | ❌ unreachable — geyser delivers post-block; shredstream unbuilt |
| Exits | ❌ not built |

See [Verification Status](./verification-status.md) for the full breakdown, and the go-live checklist
where only step 1 of 6 is complete.

## History

Three earlier branches — `test/1`, `feature/ben`, `sniper-all-tokens` — held the previous
architecture: a Rust listener bridged over a WebSocket (port 3012) to a TypeScript `swap/` service
that executed swaps via Jito. All three are **abandoned**; their hand-rolled buy transactions no
longer work on-chain. Last touched April 2025, ~16 months before this branch.

The current design collapses that two-process, two-language pipeline into a single Rust binary, which
removes the WebSocket hop, the JSON contract between halves, and the duplicated discriminator
constants that had to be maintained by hand on both sides.

## Documentation Map

- [Architecture — Sniper](./architecture-sniper.md) — the product
- [Architecture — Carbon Framework](./architecture-carbon-framework.md) — `carbon-core` 1.0.0
- [Configuration Reference](./configuration-reference.md) — every env var
- [Verification Status](./verification-status.md) — proven vs assumed vs untested
- [Source Tree Analysis](./source-tree-analysis.md)
- [Development Guide](./development-guide.md)
- [Deployment Guide](./deployment-guide.md)
