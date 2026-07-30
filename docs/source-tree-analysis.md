# Source Tree Analysis

Branch `sniper` @ `e0cad975`. Entry points marked **▶**.

```
carbon/
├── Cargo.toml                    # workspace: crates/* datasources/* decoders/* examples/* metrics/*
│                                 #   version 1.0.0, rust-version 1.82 (MSRV)
├── rust-toolchain.toml           # pinned 1.88.0  ← what actually builds
├── clippy.toml / rustfmt.toml / taplo.toml
├── pnpm-workspace.yaml / turbo.json / tsconfig.base.json / package.json
│                                 # second monorepo, for packages/
├── SNIPER_PLAN.md                # architecture rationale, why old branches are dead
├── SNIPER_HANDOFF.md             # resume guide, deployment target  (§8 has a stale claim)
│
├── examples/pumpfun-sniper/      # ══════ THE PRODUCT — 21 files, ~5,100 lines ══════
│   ├── Cargo.toml                # pumpfun-sniper-example, publish = false
│   ├── README.md                 # ⚠ STALE: v2 "not implemented", SEND_MODE described wrongly
│   ├── DISPATCH_PLAN.md          # 30-wallet fanout design, locked decisions
│   ├── PROVIDERS.md              # verified endpoints, tip accounts, rate limits
│   ├── VERIFICATION.md           # mainnet evidence for the buy account layout  ← read before live
│   ├── .env.example              # every knob (defaults differ from code — see config reference)
│   ├── deploy.sh                 # operator-driven deploy to the Ubuntu box
│   └── src/
│       ├── main.rs               # ▶ startup: config → Global → fee_config assert → preflight
│       │                         #   → blockhash refresher → dispatcher → pipeline
│       ├── config.rs             # Config::from_env — 25+ vars, SendPath, Buyer, FastProvider parse
│       ├── processor.rs          # SniperProcessor: Create/CreateV2 → 9 guards → SnipeSignal
│       │                         #   + dev_buy_lamports() discriminator scan
│       ├── dispatch.rs           # BuyDispatcher: parallel build/sign → fan out to every path
│       ├── wallets.rs            # preflight_balances — aborts startup on underfunded wallets
│       ├── pump/
│       │   ├── instructions.rs   # 913 lines — buy_exact_sol_in (18 accts),
│       │   │                     #   buy_exact_quote_in_v2 (27 accts), ATA/WSOL helpers,
│       │   │                     #   StaticAccounts::from_global.  HIGHEST-RISK FILE
│       │   ├── pdas.rs           # all PDA derivations + program IDs + buyback snapshot
│       │   └── quote.rs          # CurveState, tokens_out_for_sol, min_tokens_out
│       └── sender/
│           ├── rpc.rs            # RpcPool::spray, simulate_all
│           ├── fast.rs           # FastSenderPool, PayloadFormat (json | raw for Nozomi)
│           ├── jito.rs           # MAX_BUNDLE_SIZE=5, tip_instruction, send_bundles
│           └── tpu.rs            # 556 lines — leader schedule, gossip socket resolution,
│                                 #   pre-warmed QUIC. pool=1, 400ms timeout
│
├── crates/core/src/              # ══════ carbon-core 1.0.0 ══════
│   ├── pipeline.rs               # Pipeline, PipelineBuilder, bounded channel (default 1000)
│   ├── processor.rs              # Processor<T> — the whole extension contract, 7 lines
│   ├── datasource.rs             # Datasource, Update, DatasourceId
│   ├── instruction.rs            # InstructionDecoder, NestedInstruction, CPI re-nesting
│   ├── account.rs / account_utils.rs / account_deletion.rs / transaction.rs
│   ├── filter.rs                 # NEW in 1.0.0 — Filter, DeduplicationFilter, DatasourceFilter
│   ├── metrics.rs                # Metric, MetricsExporter (renamed from Metrics)
│   ├── collection.rs / transformers.rs / deserialize.rs / block_details.rs / error.rs
│   └── graphql/ postgres/        # NEW — output targets
│                                 # NOTE: schema.rs REMOVED — no TransactionSchema in 1.0.0
│
├── decoders/                     # 63 generated crates
│   └── pumpfun-decoder/src/      #   the one the sniper uses
│       ├── accounts/ instructions/ events/ types/ graphql/
│       └── lib.rs                #   PROGRAM_ID + decoder struct
│
├── datasources/                  # 14 producers
│   ├── yellowstone-grpc-datasource/      # ← what the sniper uses
│   ├── jito-shredstream-grpc-datasource/ # ← the unbuilt route to block 0
│   └── (helius laserstream/atlas/gpa-v2/gtfa, rpc block-subscribe/program-subscribe/
│        block-crawler/tx-crawler/gpa, jetstreamer, stream-message, validator-snapshot)
│
├── metrics/                      # log-metrics (used), prometheus-metrics
├── packages/                     # TS codegen: cli, renderer, versions  (replaced crates/cli)
├── scripts/                      # cargo-fmt.sh, cargo-clippy.sh, publish-*, bump-cli-version
├── misc/  assets/                # upstream extras
└── docs/                         # ◄── this documentation set (untracked)
```

## Critical files, in order of risk

| File | Lines | Why |
|---|---|---|
| `pump/instructions.rs` | 913 | Builds the buy. 18 accounts for v1, 27 for v2, two of them undocumented. A wrong account or flag means every buy fails. Changed most recently and most consequentially. |
| `sender/tpu.rs` | 556 | Leader tracking, gossip resolution, QUIC lifecycle. Most moving parts; two non-obvious constants (pool=1, 400 ms). |
| `config.rs` | 319 | The `SEND_MODE` dry-run gate lives here. A misread turns a dry run into live spending. |
| `dispatch.rs` | 285 | Parallel build/sign, provider assignment alignment, tip placement. |
| `processor.rs` | 258 | 9 guards + dev-buy scan. Determines what gets sniped at all. |
| `pump/pdas.rs` | 227 | Every derivation. A wrong PDA fails fast at boot for `fee_config`, silently otherwise. |
| `pump/quote.rs` | 139 | Slippage floor. Wrong math means either no fills or bad fills. |

## Reading order for a newcomer

1. `SNIPER_PLAN.md` — why this exists and why the old design was abandoned.
2. `docs/architecture-sniper.md` — the flow end to end.
3. `examples/pumpfun-sniper/src/main.rs` — startup sequence; every fail-fast gate is visible.
4. `src/processor.rs` — what qualifies as a snipe.
5. `src/dispatch.rs` — what gets built and where it goes.
6. `VERIFICATION.md` — the account-layout evidence. **Required before running live.**
7. `src/pump/instructions.rs` — only once you need to change the buy.

Skip `decoders/` entirely — generated output, 63 crates of it.

## Volume

| Area | Notes |
|---|---|
| `examples/pumpfun-sniper/` | 21 files, ~5,100 lines — **the hand-written product** |
| `crates/core/` | ~20 modules — the framework |
| `decoders/` | 63 crates, generated; treat as build output |
| `datasources/` | 14 crates |
| `packages/` | TypeScript codegen |

When estimating work, the hand-maintained surface for this product is **13 Rust source files** under
`examples/pumpfun-sniper/src/`.
