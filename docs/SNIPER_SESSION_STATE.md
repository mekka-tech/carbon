# Sniper — Session State / Pickup Guide

**Written 2026-07-30.** Branch `sniper` @ `e0cad975` + uncommitted work below.
Read this first when resuming. Nothing here is committed.

## LIVE RESULTS — 2026-07-30

**A full buy→sell round trip executed on mainnet.** Both directions verified on chain.

### Buy (4 wallets, live)
```
create_slot=436196731
LANDED buyer #0,#1,#3: slot 436196732  delta=+1
LANDED buyer #2:       slot 436196733  delta=+2
4/4 SUCCESS — ~3.39-3.45M tokens each, ~0.115 SOL spent each
```
**Block +1, not block 0.** Run from WSL locally; the Frankfurt box should improve
this. The priority-fee inversion never fired — landing at +1 means the buys
executed *after* the create, which is why they worked.

### Sell (4 wallets, live)
```
4/4 SUCCESS, all tokens -> 0
proceeds +0.0961 / +0.0980 / +0.0949 / +0.0955 SOL
```

### Round trip economics
| | SOL |
|---|---|
| Before buys | 1.9756 |
| After buys | 1.5176 |
| After sells | 1.9020 |
| **Net cost** | **-0.0736** |

~1% pump fee each way, ~0.04 priority fees, tips, ATA rents.

### Settled facts
- Buy and sell both work end to end on mainnet.
- `COMPUTE_UNIT_LIMIT=120000` was **too low** — real usage is 123k-135k. The
  working `.env` now sets `180000`; the **code default and `.env.example` are
  still 120000**, so a deployment that omits the variable inherits the bad value.
- Priority fee = `price x limit / 1e6`. Scheduling is on **price per CU**, so a
  tighter limit buys higher priority for the same spend.
- Simulation never runs on the live path (`dry_run` returns before sending), and
  all live paths already set `skipPreflight`.

## Where we got to

The sniper **fired end-to-end on a real launch for the first time**. Detection,
guards, dev-buy decode, quote, build and dispatch all executed in the same
second. The buy was rejected in *simulation* on that first attempt; the cause
was found, the fix applied, and the live round trip above followed — so the log
below is history, not the current state.

```
02:18:54  create seen: V2 { mayhem: false } mint=329R2Jmus…pump watched=true
02:18:54  SNIPE TEAM_ZERO (TZERO) sig=4q7B2e1u8Ls…
02:18:54  dispatching 4 buys via {Fast} (dev_buy=505000000 lamports)
02:18:54  SIMULATE buyer #0: FAILED InstructionError(5, IncorrectProgramId)
02:18:55  SIMULATE buyer #1..3: FAILED InstructionError(6, Custom(6042))
```

## Two bugs found and fixed this session

### 1. Shredstream decoded nothing (fixed, verified)

**Symptom:** 0 launches decoded from ~180k transactions, while the pipeline
reported `100% successful, 0 failed`.

**Cause:** shreds carry no metadata, so `TransactionStatusMeta::default()` left
`loaded_addresses` empty. `extract_instructions_with_metadata` builds its key
list as `static ++ loaded.writable ++ loaded.readonly`, so for a v0 transaction
using an Address Lookup Table every ALT-range account index fell off the end.
Instructions still extracted (program ids are in static range) but with
truncated account lists, so `ArrangeAccounts` failed and decoders returned
`None`. **No error was raised** — that is why metrics looked perfect.

Every pump.fun v2 launch is v0-with-ALT (18 static + 16 ALT accounts), so this
made the shredstream path completely non-functional for the actual goal.

**Fix:** `datasources/jito-shredstream-grpc-datasource/src/alt.rs` — fetches
lookup tables, caches by address (append-only, so hits stay valid; a short
vector after an extension triggers one refetch), fills `loaded_addresses`
before emitting. Restricted with `with_programs_of_interest([pump])` because
shredstream has no server-side filter and we would otherwise fetch lookup
tables for the entire network. Drops the transaction on partial resolution — a
misaligned key list is worse than no decode. 4 unit tests on the parser.

**Verified:** 0 → 23 launches decoded in 75s.

### 2. v2 quote overstates output ~5.8x (interim fix applied, since proven live)

**Symptom:** `AnchorError buy_v2.rs:866 BuySlippageBelowMinTokensOut (6042)`

```
Left:  3,367,478,214,920   ← tokens actually available
Right: 18,481,886,844,622  ← our min_tokens_out
```

Our computed value reproduces `Right` exactly, so the quote code is correct —
the model is wrong.

**Cause:** `Global` carries `initial_virtual_quote_reserves` (4.292 SOL) for v2
but **no v2 token reserve**. `main.rs` builds `initial_curve_v2` by overriding
only the SOL reserve and inheriting v1's `initial_virtual_token_reserves`
(1.073e15). Solving backwards from the chain's actual output, the true v2
reserve is **~1.858e14** — a 5.78x difference. No `Global` field matches that
value, so it is set per-coin on the bonding curve at create time.

Confirmed `Global` values (mainnet, 2026-07-30):

| field | value |
|---|---|
| `initial_virtual_token_reserves` | 1,073,000,000,000,000 |
| `initial_virtual_sol_reserves` | 30,000,000,000 |
| `initial_real_token_reserves` | 793,100,000,000,000 |
| `token_total_supply` | 1,000,000,000,000,000 |
| `initial_virtual_quote_reserves` | **4,292,000,000** |
| `fee_basis_points` / `creator_fee_basis_points` | 95 / 5 |

**Interim fix applied:** v2 buys now request `min_tokens_out = 1`. Slippage
protection is worthless at block 0-1 on a launch we are racing — a
wrong-but-high floor only guarantees failure. `V2_TRUST_QUOTE=true` restores
the modelled floor once the reserve is read properly.

**Proper fix (NOT done):** read the real reserves from the bonding curve
account instead of modelling them from `Global`. Note this needs an RPC read on
a path that is currently zero-RPC by design — likely fetch-after-create with
the address already carried in the `SnipeSignal`.

### 3. buyer #0 `IncorrectProgramId` — was a test artifact, not a bug

`simulate_all` awaited each simulation sequentially, so buyer #0 ran ~1s ahead
of the others and hit a mint that was not yet on chain. **The live send paths
were always parallel** (`fast.send_assigned` and `rpc.spray` both use
`join_all`; builds use `spawn_blocking` + join). Simulator is now parallel too
so dry runs match live behaviour.

## Uncommitted changes

| Path | Change |
|---|---|
| `datasources/jito-shredstream-grpc-datasource/src/alt.rs` | **new** — ALT resolution + cache, 4 tests |
| `…/jito-shredstream-grpc-datasource/src/lib.rs` | x-token interceptor, TLS, ALT wiring, program filter, `Debug` impl |
| `…/jito-shredstream-grpc-datasource/Cargo.toml` | + tonic, solana-message, solana-pubkey |
| `misc/jito-protos/protos/*.proto` | **new** — fetched from `jito-labs/mev-protos`; crate could not build without them |
| `examples/pumpfun-sniper/src/config.rs` | `DatasourceKind`, shredstream config, `synthetic_meta`, `log_all_creates`, `v2_trust_quote`, guard validation + tests |
| `examples/pumpfun-sniper/src/processor.rs` | synthetic-meta guard skipping, `LOG_ALL_CREATES` diagnostic, `TradeEvent` folding, named guard predicates + tests |
| `examples/pumpfun-sniper/src/dispatch.rs` | v2 `min_tokens_out` override, position files |
| `examples/pumpfun-sniper/src/main.rs` | datasource selection, ALT wiring, `--interactive` |
| `examples/pumpfun-sniper/src/sender/rpc.rs` | parallel simulation |
| `examples/pumpfun-sniper/src/sell.rs` | **new** — `sell_v2` accounts, `verify_layout` gate, Pyth SOL/USD, `sell_one`, `report_proceeds` |
| `examples/pumpfun-sniper/src/console.rs` | **new** — the interactive REPL and the ratatui panel |
| `examples/pumpfun-sniper/src/dashboard.rs` | **new** — panel `Snapshot`/`render`, 5s RPC refresh |
| `examples/pumpfun-sniper/src/tui_log.rs` | **new** — log ring behind the `events` pane |
| `examples/pumpfun-sniper/src/market.rs` | **new** — live per-mint market state from `TradeEvent` reserves |
| `examples/pumpfun-sniper/src/bin/sell_all.rs` | **new** — standalone sell CLI (duplicates sell logic) |
| `docs/*` | rewritten for this branch |
| `.env`, `wallets/` | gitignored — config + 4 buyer keypairs |
| `positions/` | untracked runtime output — one JSON per snipe, read by `status` / `sell` |

Tests: **91 in `pumpfun-sniper-example`** + 4 in the shredstream datasource.
The number moves every session — run
`cargo test -p pumpfun-sniper-example` rather than trusting it.

## Environment

- Shredstream: `https://prism-frankfurt.constant-k.com:55577` — **x-token required**;
  without it a bare `HTTP 204` surfaces as "malformed header: missing HTTP
  content-type", not an auth error. TLS verifies with standard roots.
- RPC: `https://rpc-frankfurt.constant-k.com/?api-key=…` (works).
  **NY endpoint TCP 443 is closed/blocked** — do not rely on it.
- Fast sender: Helius Sender Frankfurt, no API key, `/ping` 200.
- Creator watched: `78uLTjkwpsN2g93q7BkU71NTcHbYPiXyD3f7FcpK8RAt`
- 4 buyer wallets in `wallets/`, ~1.98 SOL total, all funded for 0.1 SOL buys.

## Known-good facts (verified this session, do not re-derive)

- **create + dev buy are one atomic transaction** (`create_v2` + `buy_v2`
  together). You cannot inject between them.
- A buy cannot execute before `create` — the curve PDA does not exist.
- Shredstream throughput ~3,000 tx/s, queue stays at 0 in a debug build.
- gRPC **is** reachable from this environment; the handoff's claim that a
  sandboxed session cannot carry gRPC is false here.
- pump.fun v2 = Token-2022 base mint, WSOL quote mint.

## Sell tool — `src/bin/sell_all.rs`

Self-contained CLI. Defaults to simulate-only; `--execute` sends.

```bash
SELL_MINT=<mint> SELL_CREATOR=<creator> SELL_PCT=100 \
  ./target/debug/sell_all [--execute]
```

**Safety gate:** before building anything it re-derives all 26 `sell_v2`
accounts of a known-good mainnet sell and compares address-for-address. On the
first run it correctly **refused to proceed**, catching two wrong derivations:
`sharing_config` and `fee_config` live on the **fee program**, not pump, and
`sharing-config` is hyphenated. Without that gate, four malformed transactions
would have gone out against 13.68M tokens.

`SELL_PCT` (default 100) sells a percentage of each wallet's balance.

**`min_sol_output = 1`** — exits at any price. Right for a full exit, wrong as a
default if a floor is ever wanted.

## Profit tracking — all on-chain, no third-party services

**SOL/USD comes from the Pyth pull oracle**
`7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE` (`PriceUpdateV2`), read with a
plain `getAccountInfo`.

> **Do NOT use the legacy Pyth account `H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG`.**
> It still exists and still parses cleanly, but has been frozen since ~slot 299M
> (**~635 days**) with `status = 0`. It returns a stale ~$119 that looks
> completely plausible. Real price at the time was $74.81. The code now hard-gates
> on `publish_time` age (`MAX_PRICE_AGE_SECS = 300`) and omits USD rather than
> report a stale figure.

`price` sits after a variable-length enum in `PriceUpdateV2`, so offsets 73 and
74 are both tried and validated on exponent and magnitude.

**Cost basis** comes from `positions/<mint>.json`, written by the dispatcher on
every snipe (mint, creator, curve, create slot, dev buy, buy signatures).
Proceeds are the actual on-chain SOL delta, so fees/rent/tips are already
included — no separate fee model to drift out of sync.

Still to wire: reading the position file in `sell_all` to print `%` return
alongside the absolute SOL/USD figures.

## Interactive console — `--interactive`

```bash
./target/debug/pumpfun-sniper-example --interactive   # -i also works
```

The pipeline runs in a background task while a command loop holds the
foreground, so a snipe can be watched, valued and exited in the same process
that took it.

### The panel

A ratatui full-screen panel (alternate screen, raw mode) in three parts:

1. **Header** — feed, routes, buy size, watched creators, and a red `*** LIVE
   ***` / green `dry run` banner; then the position: mint, live price, mcap,
   last-trade age, cost, value, and **PNL in SOL, % and USD** (green/red).
   Wallet rows follow: index, pubkey, SOL, token units, value, per-wallet P&L.
2. **`events`** — the log ring (`src/tui_log.rs`). Command output and pipeline
   events both land here, so nothing has to leave the panel to be read.
   `SNIPE` / `LANDED` / `SOLD` / `REALISED` are highlighted.
3. **`command`** — the prompt.

Two cadences: the panel redraws ~4x/s from in-memory market state; the
RPC-backed half (balances, cost basis, SOL/USD) refreshes every 5s, because
polling wallets at panel rate would rate-limit the same endpoint dispatch needs.
Ctrl-C or Esc exits. When stdin is not a TTY the console falls back to a plain
`sniper> ` prompt, so it stays scriptable.

### Commands

| command | effect |
|---|---|
| `watch <creator>` | arm on a creator — takes effect live, no restart |
| `unwatch <creator>` | disarm |
| `watching` | list armed creators |
| `wallets` | index, pubkey, SOL, token balance |
| `positions` | sniped positions on disk |
| `status` | cost, live price, mcap, value, **P&L in SOL / % / USD** |
| `market` | live price, mcap, volume in/out, net flow, buy/sell counts, age |
| `price` | live on-chain SOL/USD (Pyth) |
| `s <pct>` | sell pct% of **every** wallet — simulate |
| `s <wallet> <pct>` | sell pct% of **one** wallet — simulate |
| `s <wallet> <pct> go` | same, but **actually sends** |
| `help` / `quit` | |

`sell` is accepted as a synonym for `s`.

#### ⚠️ ARGUMENT ORDER — WALLET FIRST, THEN PERCENT

With two numbers, **the first is the wallet index and the second is the
percentage**:

```
s 50            50% of ALL wallets            (simulate)
s 2 50          50% of WALLET #2              (simulate)
s 2 50 go       50% of WALLET #2              — SENDS
```

There is no way for the program to catch a swapped pair: both arguments are
numbers, both pass the 0-100 range check, and both index a valid wallet. `s 2 3`
means *3% of wallet 2*, not *2% of wallet 3* — it executes silently either way.
Wallet indices are the load-order indices printed by `wallets`.

Sells are gated on `sell::verify_layout()` at startup and again on every
command, and the wallet index is range-checked against `cfg.buyers`. Sells run
concurrently across wallets (sequentially they took ~10s each and froze the
panel for the whole batch). Realised proceeds are measured on a detached task
~10s later and land in `events` as `REALISED`.

### Module layout
- `src/sell.rs` — sell_v2 accounts, layout verification, Pyth price, `sell_one`,
  `report_proceeds`.
- `src/console.rs` — the REPL and the panel.
- `src/dashboard.rs` — panel `Snapshot` + `render`, and the 5s RPC refresh.
- `src/tui_log.rs` — the log ring behind the `events` pane; filters metrics noise.
- `src/market.rs` — per-mint live market state folded from `TradeEvent`s.
- `src/bin/sell_all.rs` — standalone scripted CLI. **Duplicates the sell logic**
  because Cargo forbids `src/bin/*` importing `main.rs` modules. Fix with a
  `src/lib.rs` before either copy changes again.

## Live market tracking — the v2 curve problem is solved

**Every pump trade emits a `TradeEvent` CPI event carrying the filled amounts
AND the curve's virtual reserves after the fill.** That removes the need to model
the bonding curve at all — the chain publishes its state on every trade.

`TradeEventEvent` fields used: `mint`, `sol_amount`, `token_amount`, `is_buy`,
`virtual_sol_reserves`, `virtual_token_reserves`, `timestamp`.

Reached as `PumpfunInstruction::CpiEvent { data: CpiEvent::TradeEvent(..) }`.

`src/market.rs` folds these into per-mint state:
- price from live reserves (falls back to last fill)
- market cap and per-token price — **assumes a 1e9 supply at 6 decimals and
  reads no mint**; Token-2022 v2 coins carry their own decimals, so treat these
  two as indicative. Base-unit figures (position value, volumes) are exact.
- volume in / out, buy / sell counts, net flow
- position value for N base units

Only mints passed to `track()` accumulate — the feed carries the whole network,
so an untracked mint is dropped. A snipe starts tracking its mint automatically.
Membership is tested under a **read** lock on the detection path and the write
lock is taken only on a hit; on shredstream this code runs at full network trade
rate, so a blanket write lock there serialises detection against the panel.

Events strictly older than the newest one seen are **dropped**: reserves are
absolute curve state, not a delta, so a forked or replayed trade would otherwise
rewind the price. Equal timestamps are kept — pump stamps whole seconds and a
busy launch puts many real trades in one. A trade carrying `timestamp == 0` is
stamped with local time; "has this market got data" is the explicit `has_data`
flag, never `last_update_unix > 0`.

**This is what makes `status` able to show real unrealised P&L**: cost basis
from the buy transactions, value from live reserves, USD from Pyth. It prints
PNL in SOL, % and USD. (An earlier revision of this doc claimed `status`
deliberately omitted P&L and printed `not shown — v2 curve reserves unresolved`.
That has not been true since `market.rs` landed.)

## Snipe retry

`SNIPE_RETRIES` (default 1). After sending, polls ~4s for any landed signature;
if **zero** landed, rebuilds with a fresh blockhash and resends.

Only retries on total failure — a partial fill is a success and resending would
double the position. Each attempt pays priority fees again, so it is bounded.

## Still not built

- **`src/lib.rs`** to kill the sell-logic duplication between `src/sell.rs` and
  `src/bin/sell_all.rs` (Cargo forbids `src/bin/*` importing `main.rs` modules).
- **The console's own sell has never executed a live sale.** The real sale went
  through `bin/sell_all.rs`. The console path is simulated-only so far.
- **Block 0** — needs the Frankfurt box, or a Jito bundle for determinism.
- **v2 reserves from the bonding curve.** Until then v2 buys ship
  `min_tokens_out = 1`. Note this is only the *buy quote*: displayed price,
  mcap and P&L come from `TradeEvent` reserves and are not affected.

## Review findings that are DISPROVEN — do not re-raise

Each of these has been raised, checked against the chain or the code, and
closed. Re-opening them costs a session.

- **"The sell needs to wrap/unwrap WSOL."** It does not. Verified on chain
  across 4/4 live sells: proceeds arrive as **native SOL** and **no WSOL account
  remains** afterwards. Only the v2 *buy* wraps. Adding wrap/unwrap to the sell
  adds instructions, rent and failure modes for nothing.
- **"`create_slot` on shredstream is fabricated."** It is not. It is the real
  `Entry.slot` carried by the shred, which is why buy-slot minus `create_slot`
  is a trustworthy +1/+2 measurement. What shredstream *does* fabricate is
  `TransactionStatusMeta` (balances, status) and `block_time` — those are local
  receive time and empty defaults, which is exactly why the balance and
  freshness guards are skipped on that feed.
- **"The `MIN_CREATOR_BALANCE_SOL` guard admits every launch on shredstream."**
  Backwards. With no balances, `pre` reads 0 and `0 < minimum` is **true**, so
  the floor would reject *every* launch — a silent kill switch. That is why
  `Config::from_env` refuses the combination outright. The guard that fails
  **open** is `MAX_CREATOR_BUY_SOL` (`spent` is 0, never exceeds the ceiling),
  and `MAX_TX_AGE_MS` with it. Both directions are pinned by tests in
  `processor.rs`.

## Next steps, in order

1. Read v2 reserves from the bonding curve; re-enable `V2_TRUST_QUOTE` so buys
   get a real slippage floor instead of `min_tokens_out = 1`.
2. `src/lib.rs` to kill the sell-logic duplication before either copy changes.
3. Execute a live sell **through the console** (the live sale went through
   `bin/sell_all.rs`).
4. Block 0: Frankfurt box, or a Jito bundle for determinism.
5. Still not built: RPC-interceptor trigger, staked QUIC.

## Run it

```bash
timeout 2700 env RUST_LOG=info LOG_ALL_CREATES=true \
  ./target/debug/pumpfun-sniper-example > /tmp/live.log 2>&1 &
```

`SEND_MODE` unset = dry run. **Any other value sends real transactions.**
