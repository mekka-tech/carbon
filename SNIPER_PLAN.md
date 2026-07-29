# Pump.fun Creator-Wallet Sniper — Rebuild Plan (Rust-only)

## Goal

Listen for pump.fun coin creation by a configurable list of creator wallets and
immediately dispatch multiple buy transactions ("sniper"), rebuilt from scratch
on current `main` using Rust only — no TypeScript `swap/` service, no WebSocket
bridge.

## What the old branches did (investigated)

Three outdated branches implement versions of this, all based on a ~Feb–Apr 2025
fork of carbon with **no merge base** with current `main` (rebase is not viable):

- **`sniper-all-tokens`** (newest, Apr 2025) and **`test/1`** — the sniper:
    - `examples/alerts/src/pumpfun/pumpfun_new_tokens.rs`: a carbon
      `Processor` on `PumpfunInstruction::Create` that filtered creators by
      pre/post SOL balance (`MIN_CREATOR_BALANCE`, `MAX_CREATOR_BUY`), a creator
      blacklist, and a max-concurrent-positions counter, then pushed a JSON
      `SwapOrder` over a local WebSocket (`ws://localhost:3012`).
    - `swap/` (TypeScript): WebSocket server that received orders and built the
      pump.fun buy/sell transactions by hand (hardcoded discriminators, 12-account
      key list), sent via Jito bundles or RPC, with order book, PnL tracking,
      stop-loss/take-profit, balance guard, Discord webhooks.
    - `PumpfunInstruction::TradeEvent` was used for position tracking / exits.
- **`feature/ben`** — earlier iteration, order book + PnL only, no `swap/`.

## Why it can't be reused as-is

1. **Framework API changed.** Old code: `#[async_trait] Processor` with
   `type InputType = (InstructionMetadata, DecodedInstruction<T>, Vec<NestedInstruction>)`.
   Current core (`carbon-core` 1.0): `Processor<InstructionProcessorInputType<'_, T>>`
   with borrowed input (`input.metadata`, `input.decoded_instruction`), no
   `async_trait`. Datasource construction also changed (see
   `examples/yellowstone-grpc/src/main.rs` and its `variants.rs`).
2. **The pump.fun program changed a lot.** The old decoder had 10 instruction
   variants; the current one has ~40. Since then pump.fun added:
    - **Creator fees** — `Buy` now requires a `creator_vault` PDA (derived from
      the bonding-curve creator).
    - **Volume accumulators** — `global_volume_accumulator` +
      `user_volume_accumulator` accounts on `Buy`, and a `track_volume: OptionBool`
      arg appended to buy data.
    - **Fee config/program** — `fee_config` + `fee_program` accounts on `Buy`
      (16 accounts total now vs 12 in the old TS builder).
    - **`create_v2` / `buy_v2` / `sell_v2`** — quote-mint-generalized flow
      (WSOL as quote token, cashback, mayhem mode, buyback fee recipients).
      `Global.create_v2_enabled` gates it; new coins are created via `CreateV2`
      when enabled, so listening only for legacy `Create` misses coins.
    - Events are now emitted through a single `CpiEvent` variant
      (`CreateEvent`, `TradeEvent`, … nested inside), not top-level variants.
3. **The old TS builder is broken on-chain today** (missing creator_vault /
   volume accumulator / fee accounts, stale discriminator usage) — this is why
   "pumpfun has changed a lot" bites: buys would fail immediately.

Everything needed to rebuild is already on `main`: an up-to-date
`carbon-pumpfun-decoder` (all v1+v2 instructions, accounts incl. `Global`,
`BondingCurve`, `FeeConfig`, and all CPI events) and current datasources
(`yellowstone-grpc`, `helius-laserstream`, `jito-shredstream`).

## Architecture (new)

Single Rust binary at `examples/pumpfun-sniper/` (workspace member), two halves
connected by a `tokio::sync::mpsc` channel — the processor never blocks the
pipeline:

```
Yellowstone gRPC (processed commitment, tx filter: pump.fun program)
        │
        ▼
carbon Pipeline ── PumpfunDecoder ── SniperProcessor
        │  (filter: Create/CreateV2 by watched creator wallets,
        │   guards: blacklist, balance, max positions, tx age)
        ▼  SnipeSignal { mint, creator, bonding_curve, ... }
   mpsc channel
        ▼
   BuyDispatcher (own tokio task(s))
     ├─ builds N buy txs (one per configured buyer keypair, staggered
     │  amounts/tips) — all accounts derivable locally, no RPC fetch
     └─ sends via Jito bundle and/or plain RPC `sendTransaction`
        ▼
   PositionTracker (processor on CpiEvent::TradeEvent)
     └─ optional: PnL, take-profit/stop-loss sells (phase 2)
```

### Module layout

```
examples/pumpfun-sniper/
├── Cargo.toml
├── .env.example
├── README.md
└── src/
    ├── main.rs            # pipeline wiring, config load, task spawn
    ├── config.rs          # env config (creators, keypairs, amounts, guards)
    ├── processor.rs       # SniperProcessor: Create/CreateV2 + CreateEvent handling
    ├── pump/
    │   ├── mod.rs
    │   ├── pdas.rs        # bonding_curve, creator_vault, volume accumulators,
    │   │                  #   fee_config, event_authority PDAs
    │   ├── instructions.rs# Buy (v1) + BuyV2 builders (reuse decoder structs +
    │   │                  #   discriminators via borsh serialize)
    │   └── quote.rs       # token-out calc from initial virtual reserves + slippage
    ├── dispatch.rs        # BuyDispatcher: build/sign/send N txs, dedup per mint
    ├── sender/
    │   ├── mod.rs
    │   ├── rpc.rs         # solana-client nonblocking send (skip_preflight)
    │   └── jito.rs        # Jito block-engine bundle submit + tip ix
    └── tracker.rs         # phase 2: TradeEvent position tracking / exit sells
```

## Key implementation details

### 1. Listening (what's needed vs old code)

- Datasource: `carbon-yellowstone-grpc-datasource` with
  `SubscribeRequestFilterTransactions { account_include: [PUMPFUN_PROGRAM_ID], failed: Some(false), vote: Some(false) }`
  and `CommitmentLevel::Processed` — copy the `variants.rs` pattern from
  `examples/yellowstone-grpc` (also gives us laserstream/shredstream as
  drop-in alternatives for lower latency later).
- Match **both** `PumpfunInstruction::Create(..)` and
  `PumpfunInstruction::CreateV2(..)` (plus `CpiEvent::CreateEvent` as the
  authoritative source of `creator`, reserves, `quote_mint`, `is_mayhem_mode` —
  it fires in the same transaction and carries everything the buy needs).
- Creator matching: `HashSet<Pubkey>` of watched wallets from config, checked
  against the create instruction's `user`/`creator` account **and** fee payer.
  Keep the old guards (they were the actual edge of the old bot):
    - creator pre-balance ≥ `MIN_CREATOR_BALANCE`
    - creator's own dev-buy ≤ `MAX_CREATOR_BUY` (pre/post balance diff)
    - blacklist, max open positions, and a **tx-age gate** (skip if
      `now - block_time > TIME_DIFF_PERMITTED`, as the TS side did).

### 2. Buy transaction (the part that must be rewritten)

All accounts are derivable locally at Create time — **zero RPC fetches on the
hot path**:

- `bonding_curve` / `associated_bonding_curve`: given in the create accounts.
- `creator_vault` = PDA `["creator-vault", creator]` — creator is known from
  the create (it's the watched wallet).
- `global_volume_accumulator` = PDA `["global_volume_accumulator"]`,
  `user_volume_accumulator` = PDA `["user_volume_accumulator", buyer]`.
- `fee_config` = PDA on the pump fee program, `fee_program` = the fee program id.
- `global`, `event_authority`, `fee_recipient`: constants / fetched once at
  startup from `Global` (also gives `initial_virtual_{token,sol}_reserves` for
  the quote calc and `create_v2_enabled`, `fee_recipients` rotation).
- Instruction data: reuse the decoder's `Buy` struct + discriminator
  (`[102, 6, 61, 18, 1, 218, 235, 234]`) with borsh — no hardcoded magic
  numbers like the old TS code. Same for `BuyV2`
  (`[184, 23, 238, 97, 103, 197, 211, 61]`) when the coin was created via
  `CreateV2` with a WSOL quote (needs WSOL ATA wrap/unwrap like the old TS buy
  path).
- Quote: `tokens_out = amount_in_after_fees * virtual_token / (virtual_sol + amount_in)`
  from **initial** reserves (fresh curve) adjusted for the creator's dev buy
  (from `TradeEvent`/`CreateEvent` in the same tx), then `max_sol_cost` with
  configured slippage.
- **Verification step before coding the builders**: pull 2–3 recent successful
  third-party buy transactions from an explorer and diff their account lists
  against our builder output (exact seeds for `fee_config`/fee program will be
  confirmed here; the decoder gives us the account order).

### 3. Multi-buy dispatch ("multiple buy")

- Config: N buyer keypairs (`BUYER_KEYS`, comma-separated or dir of json files),
  per-buyer `BUY_AMOUNT` (or randomized range), `SLIPPAGE`, `PRIORITY_FEE`,
  `JITO_TIP`.
- Each buyer gets its own tx (own blockhash-signed v0 message, compute-budget
  ixs + optional Jito tip ix). Dispatch strategies (config flag):
    - `jito-bundle`: all N txs in one bundle (atomic, lands together), tip on
      the last tx.
    - `rpc-spray`: independent `send_transaction` with `skip_preflight=true`
      to one or more RPC urls.
- Blockhash kept warm by a background refresher task (poll every ~400ms) so the
  hot path never awaits an RPC.
- Per-mint dedup (the old `alreadySwappedBuy` list) + global position counter.
- `SIMULATE` mode flag for dry-runs (log the built tx, don't send).

### 4. Phase 2 (after buys work): exits

Port the old order-book/position logic as `tracker.rs`: subscribe to
`CpiEvent::TradeEvent`, compute price from event reserves, trigger
take-profit/stop-loss sells (`Sell`/`SellV2` builders — same PDA set, minus
volume accumulators). This is separable and shouldn't block the sniper MVP.

## Dependencies (workspace already pins most)

`carbon-core`, `carbon-pumpfun-decoder`, `carbon-yellowstone-grpc-datasource`,
`solana-client` 3.x (nonblocking), `solana-sdk`-family crates (keypair,
message, transaction, compute-budget), `spl-associated-token-account` +
`spl-token` (ATA create idempotent, WSOL sync/close for v2), `borsh`, `tokio`,
`serde`/`dotenvy`. Jito: plain `reqwest` JSON-RPC `sendBundle` (no heavy SDK).

## Milestones

1. **Scaffold + listener** — new example crate, pipeline wired, logs watched
   creators' Create/CreateV2 with all derived buy accounts. (compiles, runs
   against mainnet geyser)
2. **Buy builder + verification** — instruction builders, diffed against real
   on-chain buys; `SIMULATE` mode passes RPC simulation on a fresh coin.
3. **Dispatcher** — multi-wallet send via rpc-spray + jito-bundle, dedup,
   guards, warm blockhash.
4. **Phase 2** — TradeEvent tracker + auto-sell (port of old order book).

## Open questions (defaults chosen, flag if wrong)

- **v1 vs v2 buy**: plan handles both, keyed off which create variant fired.
- **Multiple buys = multiple wallets** (assumed), not N txs from one wallet —
  one wallet sending N identical buys mostly wastes fees; confirm intent.
- Old RabbitMQ/Discord integrations: dropped (were mostly commented out).
