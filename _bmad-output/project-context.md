---
project_name: 'carbon'
user_name: 'Ben'
date: '2026-07-30'
branch: 'sniper'
commit: 'e0cad975 + uncommitted'
sections_completed:
  ['technology_stack', 'language_rules', 'framework_rules', 'product_rules', 'testing_rules', 'quality_rules', 'workflow_rules', 'anti_patterns']
status: 'complete'
rule_count: 50
optimized_for_llm: true
---

# Project Context for AI Agents

_Critical rules and patterns AI agents must follow when implementing code here. Focus on unobvious
details agents would otherwise miss._

**Repo:** fork of `sevenlabs-hq/carbon` (Solana indexing framework) carrying one product — a pump.fun
creator-wallet sniper at `examples/pumpfun-sniper/`. Full docs: `docs/index.md`.

**Branch `sniper` is the only live branch.** `test/1`, `feature/ben`, `sniper-all-tokens` are abandoned
(April 2025) — their WebSocket + TypeScript `swap/` architecture is dead and their buy transactions no
longer work on-chain. Ignore anything you find referencing `swap/`, `ws/`, `clean.js`, or port 3012.

---

## Technology Stack & Versions

- **Rust 1.88.0** — pinned in `rust-toolchain.toml`. `Cargo.toml` says `rust-version = "1.82"`; that
  is the MSRV claim, not the build toolchain. Both are intentional.
- **`carbon-core` 1.0.0** — API differs substantially from 0.6.x. See framework rules below.
- **Solana crates 3.1.5** — split crates (`solana-pubkey`, `solana-instruction`, `solana-message`,
  `solana-transaction`, `solana-signer`, `solana-system-interface`, `solana-compute-budget-interface`),
  not the monolithic `solana-sdk`.
- Tokio (full) · `futures` · Borsh · `reqwest` · `rustls` + `aws-lc-rs`
- `carbon-yellowstone-grpc-datasource` + `yellowstone-grpc-proto` 10.x · `carbon-pumpfun-decoder` 1.0.0
- 63 decoders · 14 datasources · 2 metrics exporters
- Codegen CLI is **TypeScript** under `packages/{cli,renderer,versions}` (pnpm + turbo). `crates/cli`
  no longer exists.

## Critical Implementation Rules

### Framework — carbon-core 1.0.0 (breaking vs 0.6.x)

Pre-1.0 Carbon knowledge will not compile. The changes:

- **`Processor` is generic, not associated-type.**
  `trait Processor<T> where T: Sync { fn process(&mut self, data: &T) -> impl Future<Output = CarbonResult<()>> + Send }`
  — takes `&data`, has **no metrics argument**, uses RPITIT. `SniperProcessor` is the in-repo reference.
- **Decoded instructions are struct variants carrying accounts**:
  `PumpfunInstruction::Create { data, accounts, .. }`. No separate `ArrangeAccounts` call needed.
- **`Metrics` → `MetricsExporter`** (plus a `Metric` trait). `.metrics(Arc<dyn MetricsExporter>)`.
- **Transaction schemas are gone.** `schema.rs`, `TransactionSchema`, and the `schema!` macro no longer
  exist. `.transaction(processor)` takes no schema argument.
- **The update channel is bounded** — `DEFAULT_CHANNEL_BUFFER_SIZE = 1_000`, tunable via
  `.channel_buffer_size(n)`. 0.6.x was unbounded; do not repeat the "no backpressure" claim.
- **New:** `filter.rs` (`Filter`, `DeduplicationFilter`, `DatasourceFilter`, `*_with_filters` builder
  methods), `DatasourceId` / `.datasource_with_id`, `graphql/` and `postgres/` output targets.

Still true from 0.6.x:

- Decoders **short-circuit on identity first** — `program_id != PROGRAM_ID → None`,
  `account.owner != PROGRAM_ID → None`.
- **Account decoding tries variants sequentially; first successful Borsh decode wins.** Declaration
  order in `accounts/mod.rs` is semantically load-bearing.
- **Instruction pipes recurse** — a processor fires at every CPI depth, not just top level.
- **A datasource whose `consume` errors is logged and dropped**; the pipeline continues silently with
  one fewer producer.

### Product — pump.fun sniper

- **`SEND_MODE` is a dry-run gate, NOT a route selector.**
  `dry_run = matches!(SEND_MODE, "" | "simulate")`. **Any other value sends real transactions** —
  including `rpc`, `jito`, or a typo. Routes come from `SEND_PATHS` (`rpc,fast,jito,tpu`). The branch
  README is wrong about this and following it goes live.
- **`create_v2` IS implemented.** `SNIPE_V2` defaults to **on**. `README.md` and `SNIPER_HANDOFF.md`
  §8 both claim otherwise — they are stale. v2 = Token-2022 + WSOL quote mint,
  `buy_exact_quote_in_v2`, 27 accounts, needs SOL wrapping.
- **v1 buy takes 18 accounts, not 16.** Two arrive via `remaining_accounts` and appear in **neither the
  IDL nor the decoder**: `remaining[0]` = `bonding_curve_v2` (read-only), `remaining[1]` = a
  `Global.buyback_fee_recipients` member (**writable**). Wrong → 6062 / 6074 / 6057 /
  `PrivilegeEscalation`. **A pump.fun upgrade can change these with no compile error and no test
  failure.** Re-run `VERIFICATION.md` after any upgrade.
- **Buyback recipients come from the live `Global` account** via `StaticAccounts::from_global`, not the
  compiled-in `BUYBACK_FEE_RECIPIENTS` snapshot — `update_buyback_config` rotates them and a stale list
  fails every buy with 6057.
- **`global_volume_accumulator` must be read-only.** Writable serialises you against every other pump
  buyer in the slot.
- **Zero RPC on the hot path** is a hard invariant. All PDAs derived locally; blockhash, TPU
  connections, and Jito tip floor kept warm by background tasks. Never add an `await` on RPC inside
  `dispatch()` or `build_buy_tx()`.
- **`sniped_mints` never shrinks** — it is both the dedup set and the position counter, so
  `MAX_POSITIONS` is a **process-lifetime** budget, not a concurrent cap.
- **`try_send` on the signal channel drops on full** (1,024 slots) and logs — deliberate, so a full
  dispatcher never stalls the geyser pipeline.
- **Provider assignments must stay index-aligned with surviving transactions.** A failed build shifts
  every later index; `dispatch()` pushes to `txs` and `assignments` together for this reason.
- **Jito bundles cap at 5** and Jito rate-limits **1 req/s per region per IP** — 30 buys become 6
  bundles dealt across 6 regions. Tip rides `i % 5 == 4 || i == n-1`.
- **A wrong fast-provider tip account is accepted and then silently never lands.** Helius Sender uses
  its own tip accounts, not Jito's.
- **Nozomi is not JSON-RPC** — `sendTransaction2` takes bare base64 as `text/plain` and returns no
  signature. That is what `PayloadFormat::Raw` exists for.
- **`PRIORITY_FEE_JITTER` defaults to 0**, which makes all 30 wallets bid identically and lose the same
  tiebreak. `.env.example` sets `500000`.
- **Only top-level instructions are scanned** for the creator's dev buy — a CPI dev buy is missed and
  the `MAX_CREATOR_BUY_SOL` fallback used.
- **`chrony` is mandatory on the box.** The freshness gate compares `block_time` against the local
  clock, so drift silently kills or admits snipes with no error.
- **There IS an exit path** (`src/sell.rs`, `src/console.rs`, `src/bin/sell_all.rs`) and it has run
  live. Earlier revisions of this file said the opposite — ignore that. `sell_v2` takes **26 accounts**
  and is gated by `sell::verify_layout()`, which diffs the derivation against a known-good mainnet sell
  and refuses to build anything if it disagrees. Keep that gate in front of every new sell path.
- **The sell needs no WSOL wrap/unwrap** — verified on chain: proceeds arrive as native SOL and no WSOL
  account is left behind. Only the v2 *buy* wraps. Do not add wrap/unwrap instructions to a sell.
- **`min_sol_output = 1` on sells** — exits at any price. Right for a full exit, wrong as a default if a
  floor is ever wanted.
- **The console's sell parses `s <wallet> <pct>` — wallet FIRST.** Both arguments are numbers in the
  same range, so a swapped pair parses cleanly, passes the range check, and sells the wrong amount from
  the wrong wallet with no error. `go` is the only token that sends.
- **Never model the bonding curve for display.** Price, market cap and unrealised P&L come from the
  `virtual_*_reserves` the chain publishes on every `TradeEvent` CPI event (`src/market.rs`), because
  the v2 model built from `Global` overstates output ~5.8x. The *buy quote* still uses the model, which
  is why v2 buys ship `min_tokens_out = 1` unless `V2_TRUST_QUOTE=true`.
- **`market.rs` whole-token figures assume 1e9 supply at 6 decimals and read no mint.** Token-2022 v2
  coins carry their own decimals, so per-token price and mcap can be off by orders of magnitude. Base
  unit figures (position value, volumes) are unaffected — prefer them when the number matters.

### Language-Specific Rules — Rust

- **Bare `+ - *` on integers fails the lint gate.** `scripts/cargo-clippy.sh` denies
  `clippy::arithmetic_side_effects`; use `checked_*` / `saturating_*` / `wrapping_*`. This matters
  everywhere in the sniper — lamports, reserves, fee ladders.
- Also denied: `default_trait_access`, `manual_let_else`, `used_underscore_binding`, all warnings.
- **One braced `use { .. }` block per file** — enforced by `rustfmt.toml` (`group_imports = "One"`,
  `imports_granularity = "One"`), not style preference.
- Return `CarbonResult<T>`; `Error::Custom(String)` for sniper failures. `Config::from_env` returns
  `Result<_, String>`.
- Comment *why*, not *what*. Non-obvious constants carry their rationale inline (QUIC pool = 1 because
  the cache picks a random member; 400 ms timeout because QUIC gives no application ack). Match this.

### Testing Rules

- Inline `#[cfg(test)] mod tests` in the module under test. All passing; the count moves every session
  (91 in `pumpfun-sniper-example` + 4 in `carbon-jito-shredstream-grpc-datasource` at the time of
  writing) — **run `cargo test -p pumpfun-sniper-example` rather than trusting a number in a doc.**
- **The valuable tests assert against real mainnet fixtures**, not synthetic data —
  `buy_reproduces_a_real_mainnet_instruction` rebuilds a buy and diffs it account-for-account against a
  known-good transaction.
- **When touching `pump/instructions.rs` or `pump/pdas.rs`, add a fixture-based test.** A layout change
  that compiles and passes synthetic tests can still fail every live buy — exactly what the 16-vs-18
  account bug did.
- Tests cannot catch a pump.fun program upgrade; only re-running `VERIFICATION.md` can.

### Code Quality & Style Rules

- `rustfmt.toml`: `comment_width = 80`, `wrap_comments = true`.
- `taplo.toml`: `reorder_keys = true` on dependency tables.
- **Never hand-edit `decoders/`** — 63 generated crates. Change `packages/renderer` instead.
- Decoder layout now includes `events/` and `graphql/` subdirs, and per-instruction `postgres/` rows.

### Development Workflow Rules

- **Always scope cargo with `-p pumpfun-sniper-example`.** A bare build compiles all 63 decoders.
- **Never read a piped command's exit code as the command's.** `cargo … | tail` gives you `tail`'s
  status and a hard failure looks like success — use `${PIPESTATUS[0]}`. Likewise `grep -c` exits 1 on
  zero matches, so `grep -c … && next` silently skips `next`.
- Conventional commits on this branch: `feat(sniper):`, `fix(sniper):`, `docs:`, `chore(sniper):`.
- Run `./scripts/cargo-fmt.sh` and `./scripts/cargo-clippy.sh` before pushing.
- **Whether a session can reach gRPC varies — test, do not assume.** The blanket claim that a sandboxed
  session cannot carry gRPC/HTTP-2 proved false in the 2026-07-30 session, which streamed shredstream
  at ~3,000 tx/s from WSL. Build/test/clippy always work locally; latency-sensitive runs still want the
  Frankfurt box.

### Critical Don't-Miss Rules

- **It is LIVE and has spent real SOL.** 4/4 buys and 4/4 sells executed on mainnet (2026-07-30), a
  full round trip at ~0.115 SOL per wallet. Anything you change on the dispatch, build or sell path can
  now lose money. `SEND_MODE` unset = dry run; **any other value sends.**
- **Block 0 is unreachable via geyser.** At `Processed` a create is delivered *after* its block is
  built, so block 1 is the floor there. **Shredstream is built and working** — see
  `datasources/jito-shredstream-grpc-datasource` (ALT resolution in `alt.rs` is what made it decode at
  all; without `loaded_addresses` every v0-with-ALT pump launch silently decoded to nothing). Block 0
  still has not been achieved in practice: the live buys landed at **+1 slot**, from WSL.
- **Startup aborts on any underfunded wallet** — but only when not in dry run.
- **Deploy to Frankfurt-adjacent (FSN1), never Helsinki** — Helsinki adds ~25 ms to every snipe.
- **Unstaked QUIC is deprioritized** by validators exactly under contested-launch load.
- Do not trust `.env.example` as a mirror of code defaults — `PRIORITY_FEE_MICRO_LAMPORTS`,
  `PRIORITY_FEE_JITTER`, `JITO_TIP_SOL`, and `SEND_PATHS` all differ. See
  `docs/configuration-reference.md`.
- **`COMPUTE_UNIT_LIMIT=120000` is too low for a real v2 buy** — measured usage is 123k-135k. The
  working `.env` sets `180000`; the code default and `.env.example` are still `120000`. A fresh
  deployment that omits the variable will build buys that run out of compute.

---

## Usage Guidelines

**For AI Agents:**

- Read this file before implementing any code
- Follow ALL rules exactly as documented
- When in doubt, prefer the more restrictive option
- Verify claims in `README.md` / `SNIPER_HANDOFF.md` against the code — both contain stale claims
- **If a rule here contradicts the code, the code wins — fix the rule in the same change.** Four rules
  in the 2026-07-29 revision (no exit path, never sent a transaction, shredstream unbuilt, 36 tests)
  had gone false and would have forbidden shipped code

**For Humans:**

- Keep this lean and focused on agent needs
- Update when the stack or the buy layout changes
- Re-verify after any pump.fun program upgrade
- Remove rules that become obvious over time

Last Updated: 2026-07-30 (branch `sniper` @ `e0cad975` + uncommitted TUI/sell work). Current session
state, live results and the console reference live in `docs/SNIPER_SESSION_STATE.md`.
