# Architecture — pump.fun Sniper

**Path:** `examples/pumpfun-sniper/` · **Binary:** `pumpfun-sniper-example` · 21 files, ~5,100 lines

## Executive Summary

A single Rust binary. It subscribes to pump.fun transactions over Yellowstone gRPC at `Processed`
commitment, watches for coin creations by a configured set of creator wallets, and on a match fires
one buy transaction per configured wallet — 30 by design — concurrently across every enabled send
route. The goal is to land in the create's own block ("block 0") or the next one.

The controlling design constraint is **zero RPC calls on the hot path**. Everything a buy needs is
either derived locally (all PDAs) or kept warm by a background task (blockhash, TPU connections,
Jito tip floor). The only network I/O between seeing a create and sending is the send itself.

## Pipeline

```
Yellowstone gRPC (Processed, account_include = pump program)
        │
        ▼
carbon_core::Pipeline ── .instruction(PumpfunDecoder, SniperProcessor)
        │
        ▼
SniperProcessor::process          ← 7 guards, see below
        │  SnipeSignal
        ▼
mpsc::channel(1_024)              ← try_send: a full queue never stalls geyser
        │
        ▼
BuyDispatcher::run  ──────────────────────────────────────────────┐
        │                                                          │
   build N txs in parallel (spawn_blocking, one per buyer)         │
        │                                                          │
        ├─ V1: create_ata_idempotent → buy_exact_sol_in            │
        └─ V2: create WSOL ATA → transfer → sync_native            │
               → create Token-2022 ATA → buy_exact_quote_in_v2     │
               → [close_account if UNWRAP_AFTER_BUY]               │
        │                                                          │
        ▼                                                          │
   join_all over every enabled path, all sending every tx ◄────────┘
        │
        ├─ rpc    RpcPool::spray       — every endpoint, skip preflight
        ├─ fast   FastSenderPool       — anti-MEV providers, round-robin per wallet
        ├─ jito   send_bundles         — bundles of 5 dealt across 6 regions
        └─ tpu    TpuSender::send      — direct QUIC to upcoming leaders
```

Duplicate delivery across routes is free and deliberate: **a signature can land at most once**, so
the fastest route wins and the rest are no-ops. A tip only costs anything if that particular
transaction is the one that lands.

## Startup sequence (`src/main.rs`)

Order matters — each step is a fail-fast gate before any money is at risk.

1. `dotenv` + `env_logger`; install the rustls aws-lc-rs crypto provider (required, see rustls#1877).
2. `Config::from_env()` — every knob, validated. Errors are fatal.
3. Build `RpcPool`, `FastSenderPool` (warmed), `TpuSender` (constructed unconditionally, active only
   if `SEND_PATHS` includes `tpu`; pre-resolved and pre-warmed once so the first snipe of the run
   isn't the one paying for a QUIC handshake, then a background refresher spawns).
4. **Fetch the pump `Global` account.** Supplies fee bps, initial virtual reserves, `fee_recipient`,
   and the live `buyback_fee_recipients` set.
5. **Assert the `fee_config` PDA exists on-chain.** If the derivation is wrong, fail at boot rather
   than lose every buy.
6. Build `CurveState` for v1 (`initial_virtual_sol_reserves`) and v2
   (`initial_virtual_quote_reserves`). Warn if `SNIPE_V2` is on but the v2 reserve is 0.
7. **Preflight every buyer balance.** Any underfunded wallet aborts startup unless `dry_run`.
8. Fetch a blockhash and spawn a refresher every **400 ms**.
9. Spawn `BuyDispatcher::run`.
10. Build the carbon pipeline with `ShutdownStrategy::Immediate` and run.

## Guards (`src/processor.rs`)

`SniperProcessor` matches `PumpfunInstruction::Create` and `CreateV2`, normalises both into an
internal `Launched` struct, then applies these in order. Any failure returns early.

| # | Guard | Rule |
|---|---|---|
| 1 | v2 gate | `Launch::V2` and `!snipe_v2` → skip (warns if the creator was watched) |
| 2 | creator watch | `creator`, `user`, **or** `tx.fee_payer` must be in `WATCHED_CREATORS` |
| 3 | blacklist | `creator` or `user` in `BLACKLISTED_CREATORS` → skip |
| 4 | balance sanity | `post_balances[0] > pre_balances[0]` → skip (fee payer gained SOL) |
| 5 | min creator balance | `pre < MIN_CREATOR_BALANCE_SOL` → skip |
| 6 | max dev buy | `pre - post > MAX_CREATOR_BUY_SOL` → skip |
| 7 | freshness | `now_ms - block_time*1000 > MAX_TX_AGE_MS` → skip (stale replay) |
| 8 | dedup | mint already in `sniped_mints` → skip |
| 9 | position cap | `sniped_mints.len() >= MAX_POSITIONS` → skip |

Guard 7 is why **`chrony` is mandatory on the deployment box**: the gate compares block time against
the local clock, so clock drift silently kills or admits snipes.

`sniped_mints` is a `HashSet` that **only grows** — it is both the dedup set and the position
counter. With no exit path, `MAX_POSITIONS` is effectively a per-process lifetime budget, not a
concurrent-position limit.

### Dev-buy detection

`dev_buy_lamports()` scans the create transaction's top-level instructions for a pump.fun buy and
extracts the creator's spend, used to shift the quote (the creator's own buy moves the curve before
ours lands). It recognises four discriminators and reads the field at the right offset for each:

| Instruction | Discriminator | Field read | Offset |
|---|---|---|---|
| `buy` | `[102,6,61,18,1,218,235,234]` | `max_sol_cost` (upper bound) | 16..24 |
| `buy_v2` | `[184,23,238,97,103,197,211,61]` | `max_sol_cost` | 16..24 |
| `buy_exact_sol_in` | `[56,252,116,8,158,223,205,95]` | `spendable_sol_in` (exact) | 8..16 |
| `buy_exact_quote_in_v2` | `[194,171,28,70,104,77,91,47]` | `spendable_quote_in` | 8..16 |

Falls back to `MAX_CREATOR_BUY_SOL` when no buy is found — the conservative direction, since a larger
assumed dev buy lowers our expected token count and therefore our `min_tokens_out` floor.

> Only **top-level** instructions are scanned (`tx.message.instructions()`), so a dev buy performed
> via CPI from another program would be missed and the fallback used.

## Dispatch (`src/dispatch.rs`)

1. Build per-coin accounts once (`CoinAccounts` for v1, `CoinAccountsV2` for v2), wrap in `Arc`,
   share across all buyers.
2. Snapshot the warm blockhash.
3. For each buyer index, `spawn_blocking(build_buy_tx)` — signing 30 transactions serially would add
   avoidable milliseconds, and signing is CPU-bound, so it belongs off the async executor.
4. Collect results. **Provider assignments are pushed alongside surviving transactions**, because a
   failed build would otherwise shift every later index and misroute tips.
5. If `dry_run`, `simulate_all` and return.
6. Otherwise `join_all` over every enabled path.

### Transaction shape

```
set_compute_unit_limit(COMPUTE_UNIT_LIMIT)
set_compute_unit_price(base + jitter * i / (n-1))     ← per-wallet, deterministic
─── V1 ───────────────────────────────────────────
create_ata_idempotent(buyer, mint)
buy_exact_sol_in(statics, coin, buyer, lamports, min_tokens_out, track_volume)
─── V2 ───────────────────────────────────────────
create_ata_idempotent_with_program(quote_mint, quote_token_program)
system::transfer(buyer → quote_ata, lamports)
sync_native(quote_ata)
create_ata_idempotent_with_program(base_mint, base_token_program)   ← Token-2022
buy_exact_quote_in_v2(statics, coin, buyer, lamports, min_tokens_out)
[close_account(quote_ata) if UNWRAP_AFTER_BUY]
──────────────────────────────────────────────────
[fast provider tip, if this wallet was dealt one]
[jito tip, on every 5th wallet and the last]
```

**Priority-fee laddering:** `base_fee + jitter * i / (n-1)`. The 30 buys deliberately occupy a
*range* of priority levels rather than tying at one price, so they don't all lose the same tiebreak.

**Jito tip placement:** `i % 5 == 4 || i == n-1` — the tip rides the last transaction of each bundle
of 5, plus the final partial bundle. 30 buys → 6 bundles → 6 tips.

## Send paths (`src/sender/`)

| Path | Module | Behaviour |
|---|---|---|
| `rpc` | `rpc.rs` | `RpcPool::spray` — every tx to every endpoint in `RPC_URLS`, preflight skipped |
| `fast` | `fast.rs` | Wallets dealt round-robin across `FAST_PROVIDERS`; tip baked into that wallet's tx. Supports JSON-RPC and `raw` base64 bodies (`PayloadFormat`) because **Nozomi's `sendTransaction2` takes bare base64 as `text/plain` and returns no signature** |
| `jito` | `jito.rs` | `MAX_BUNDLE_SIZE = 5`; bundles dealt across 6 regional block engines, Frankfurt first |
| `tpu` | `tpu.rs` | Tracks the leader schedule, resolves each upcoming leader's TPU/QUIC socket from gossip, keeps connections pre-warmed. Refreshed every 400 ms; every failure degrades to keeping the previous target set |

### TPU path constants worth knowing

- **Connection pool pinned to 1.** The cache picks a random pool member per send, so a larger pool
  could hand you an unwarmed connection.
- **400 ms per-leader send timeout.** QUIC gives no application-level ack, so a black-holed peer
  would otherwise stall the dispatcher past the slot being raced for.

## Quote math (`src/pump/quote.rs`)

`CurveState { virtual_sol_reserves, virtual_token_reserves, protocol_fee_bps, creator_fee_bps }`.

- `tokens_out_for_sol(spendable_sol_in)` — the program's own constant-product formula.
- `after_buy(spendable_sol_in)` — the curve state following a buy, used to apply the dev-buy shift.
- `min_tokens_out(curve, lamports, dev_buy_lamports, slippage_bps)` — shift by the dev buy, then
  reduce by slippage. This is the on-chain slippage floor.

v2 uses identical arithmetic; only the opening reserve differs
(`initial_virtual_quote_reserves` instead of `initial_virtual_sol_reserves`).

## PDAs (`src/pump/pdas.rs`)

All derived locally, no RPC: `global`, `event_authority`, `creator_vault(creator)`,
`global_volume_accumulator`, `user_volume_accumulator(user)`, `fee_config`, `bonding_curve(mint)`,
`bonding_curve_v2(mint)`, `bonding_curve_v2_mayhem(mint)`, `sharing_config(base_mint)`.

Constants: pump program (re-exported from the decoder), fee program, Token, Token-2022, ATA program,
WSOL mint, mayhem program, and a compiled-in `BUYBACK_FEE_RECIPIENTS: [Pubkey; 8]` snapshot.

> The compiled-in buyback snapshot is a **fallback only**. `StaticAccounts::from_global(&global)`
> takes the live set at startup, because `update_buyback_config` can rotate them and a stale list
> fails every buy with `BuybackFeeRecipientNotAuthorized` (6057).

## The 18-account buy

The highest-risk area in the codebase, and the subject of `VERIFICATION.md`.

The deployed program requires **18 accounts** for `buy_exact_sol_in`. The builder originally passed
16 — every live buy would have failed with `BuybackFeeRecipientMissing` (6062). The two extra
accounts come from `remaining_accounts` and appear in **neither the IDL nor the Codama decoder**:

| Slot | Account | Constraint | Error if wrong |
|---|---|---|---|
| `remaining[0]` | `bonding_curve_v2`, PDA `["bonding-curve-v2", mint]` | read-only | `InvalidBondingCurveV2` (6074) |
| `remaining[1]` | one of `Global.buyback_fee_recipients` | **writable** | read-only → `PrivilegeEscalation`; outside set → 6057 |

Also corrected: `global_volume_accumulator` is now read-only per the IDL (a write lock there
serialises us against every other pump buyer in the slot), and the token program is a field rather
than a hardcode.

**v2 differs:** `buy_exact_quote_in_v2` takes **27 accounts**, and the trailing `bonding_curve_v2` is
*optional* because v2 promotes `buyback_fee_recipient` to a named account. It is appended only for
`is_mayhem_mode` coins, deriving on the mayhem program.

## Not built

- **Shredstream detection** — the only route to block 0.
  `carbon-jito-shredstream-grpc-datasource` is already in the workspace.
- **Exits** — no position tracking, no auto-sell. The plan is to port the legacy order book onto
  `CpiEvent::TradeEvent`.
- **`buy_v2`** (exact-tokens-out) — only the exact-quote-in variant exists, which is what a sniper
  wants.
- **Ops** — no systemd unit, latency probe, or metrics beyond `LogMetrics`.

## Failure modes to know

- **`sniped_mints` never shrinks.** `MAX_POSITIONS` is a lifetime budget; the process must be
  restarted to snipe again once reached.
- **`try_send` drops signals** when the 1,024-slot channel is full, logging an error. Deliberate —
  better than stalling the geyser pipeline — but a burst beyond capacity silently loses snipes.
- **No retry on a failed build.** A panicking or erroring `build_buy_tx` logs and that wallet simply
  doesn't participate.
- **Blockhash can be up to 400 ms stale**, plus dispatch time, against a ~60-slot expiry window.
  Not tight, but it is the reason the refresher exists.
