# carbon / pump.fun sniper — Documentation Index

**Source of truth.** Generated 2026-07-29 from branch `sniper` @ `e0cad975`
(tracking `origin/claude/pumpfun-coin-sniper-mvwiwh`).

> Point BMAD workflows (PRD, architecture, dev-story) at this file.

## What this repo is now

A fork of [`sevenlabs-hq/carbon`](https://github.com/sevenlabs-hq/carbon) — a Solana indexing
framework — carrying **one product**: a pump.fun creator-wallet sniper at `examples/pumpfun-sniper/`.

The sniper watches a list of creator wallets over Yellowstone gRPC and, the instant one of them
launches a coin, fires **30 independent buy transactions from 30 wallets** across up to four send
routes, racing to land in the same block as the create.

## Quick reference

| | |
|---|---|
| **Product** | `examples/pumpfun-sniper/` — 21 files, ~5,100 lines |
| **Binary** | `cargo run --release -p pumpfun-sniper-example` |
| **Framework** | `carbon-core` **1.0.0** |
| **Toolchain** | Rust **1.88.0** (pinned, `rust-toolchain.toml`) |
| **Solana crates** | 3.1.5 |
| **Decoders** | 63 |
| **Data source** | Yellowstone gRPC, `Processed` commitment |
| **Tests** | **36 passing** (verified this session) |
| **Live status** | **never sent a transaction** |

## Documents

| Doc | Contents |
|---|---|
| [Project Overview](./project-overview.md) | Repo shape, stack, what changed from the old architecture |
| [Architecture — Sniper](./architecture-sniper.md) | Listen → guard → build → dispatch, in detail |
| [Architecture — Carbon Framework](./architecture-carbon-framework.md) | `carbon-core` 1.0.0 API and pipeline internals |
| [Configuration Reference](./configuration-reference.md) | Every env var, verified against `config.rs` |
| [Verification Status](./verification-status.md) | What is proven, what is assumed, what is untested |
| [Source Tree Analysis](./source-tree-analysis.md) | Annotated tree and reading order |
| [Development Guide](./development-guide.md) | Build, test, lint, conventions |
| [Deployment Guide](./deployment-guide.md) | The Hetzner box, go-live checklist |

## Documents shipped on the branch

These were written by whoever built the sniper. **Two contain stale claims** — see
[Verification Status](./verification-status.md) §Doc defects.

| Doc | Contents | State |
|---|---|---|
| `SNIPER_PLAN.md` | Architecture rationale, why the old branches are dead | current |
| `SNIPER_HANDOFF.md` | Resume guide, deployment target, go-live checklist | **§8 stale** |
| `examples/pumpfun-sniper/DISPATCH_PLAN.md` | 30-wallet fanout design, locked decisions | current |
| `examples/pumpfun-sniper/PROVIDERS.md` | Verified endpoints, tip accounts, rate limits | current |
| `examples/pumpfun-sniper/VERIFICATION.md` | Mainnet evidence for the buy account layout | current |
| `examples/pumpfun-sniper/README.md` | How to run | **stale — see below** |

## Read this before running anything

**1. `SEND_MODE` is not a send-path selector, and the README is wrong about it.**

```rust
let dry_run = matches!(env::var("SEND_MODE").unwrap_or_default().as_str(), "" | "simulate");
```

Only unset or `simulate` is a dry run. **Any other value sends real transactions.** The branch README
says to use `SEND_MODE=rpc`, which would go live while you believe you selected a route. Send routes
are chosen with `SEND_PATHS` (`rpc,fast,jito,tpu`). The unset default is safe; a typo is not.

**2. `create_v2` IS implemented**, despite `README.md` and `SNIPER_HANDOFF.md` §8 both saying it
isn't. `SNIPE_V2` defaults to **on**. v2 was ~21% of sampled live buy traffic.

**3. It cannot sell.** No position tracking, no exit. It buys and holds. Phase 2, unbuilt.

**4. It has never executed a live snipe** — not even at 0.001 SOL. Tests and mainnet *simulation*
are the only evidence the buys work.

## Findings worth knowing up front

1. **The 16-vs-18 account bug** — the buy builder passed 16 accounts where the deployed program
   requires 18. Every live buy would have failed with `BuybackFeeRecipientMissing` (6062). Caught by
   mainnet differential simulation, fixed in `4e910d8e`, now pinned by tests.
2. **Two of those 18 accounts are undocumented** — `bonding_curve_v2` and a buyback fee recipient
   arrive via `remaining_accounts` and appear in neither the IDL nor the decoder. A pump.fun upgrade
   can change them with no compile error and no test failure. → [Verification Status](./verification-status.md)
3. **Block 0 is unreachable today.** Geyser at `Processed` delivers a create *after* its block is
   built, so block 1 is the floor. Shredstream is the only route to block 0 and is unbuilt.
4. **Buyback recipients are read from the live `Global` account**, not the compiled-in snapshot,
   because `update_buyback_config` can rotate them and a stale list fails every buy with 6057.
5. **Jito rate-limits 1 req/s per region per IP** — 30 buys become 6 bundles dealt across 6 regions.
6. **A wrong fast-provider tip account is accepted and then silently never lands.** Helius Sender
   uses its own tip accounts, not Jito's.
7. **Unstaked QUIC is deprioritized** by validators exactly under the load a contested launch creates.

## Superseded work

`test/1`, `feature/ben`, and `sniper-all-tokens` held the previous architecture: a Rust listener
bridged over a WebSocket (port 3012) to a TypeScript `swap/` service. All three are **abandoned** —
their hand-rolled buy transactions no longer work on-chain. Last touched April 2025.

If you need that history, `test/1` still exists and its uncommitted state is in `stash@{0}`.
