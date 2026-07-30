# Architecture — Carbon Framework (`carbon-core` 1.0.0)

The indexing framework the sniper is built on. This documents the **1.0.0 API**, which differs
substantially from 0.6.x — if you have older Carbon knowledge, read the breaking-changes table first.

## Breaking changes vs 0.6.x

| Area | 0.6.x | **1.0.0** |
|---|---|---|
| `Processor` | associated `type InputType`; `process(&mut self, data: InputType, metrics: Arc<MetricsCollection>)` | **generic** `Processor<T> where T: Sync`; `process(&mut self, data: &T) -> impl Future` (RPITIT, no metrics arg) |
| Metrics trait | `Metrics` | **`MetricsExporter`** (plus a `Metric` trait); `.metrics(Arc<dyn MetricsExporter>)` |
| Update channel | `unbounded_channel` — no backpressure | **bounded** `mpsc::channel`, `DEFAULT_CHANNEL_BUFFER_SIZE = 1_000`, tunable via `.channel_buffer_size(n)` |
| Transaction schemas | `.transaction(processor, Option<TransactionSchema<T>>)`; `schema.rs`, `schema!` macro | **removed** — `schema.rs` is gone; `.transaction(processor)` takes no schema |
| Decoded instructions | tuple variants (`PumpfunInstruction::Buy(Buy)`), accounts via separate `ArrangeAccounts` call | **struct variants carrying accounts**: `PumpfunInstruction::Create { data, accounts, .. }` |
| Filtering | none | `filter.rs` — `Filter` trait, `DeduplicationFilter`, `DatasourceFilter`, `*_with_filters` builder methods |
| Datasource identity | — | `DatasourceId`, `.datasource_with_id(...)`; channel carries `(Update, DatasourceId)` |
| Output targets | — | `graphql/` and `postgres/` modules; decoders emit matching schema/row types |

**Consequence:** any Carbon example, blog post, or LLM recollection predating 1.0.0 will not compile.
The sniper's `SniperProcessor` is the in-repo reference for the current trait shape.

## Core module map (`crates/core/src/`)

| File | Contents |
|---|---|
| `pipeline.rs` | `Pipeline`, `PipelineBuilder`, `ShutdownStrategy`, run loop, `DEFAULT_CHANNEL_BUFFER_SIZE` |
| `processor.rs` | The `Processor<T>` trait — seven lines, the whole extension contract |
| `datasource.rs` | `Datasource` trait, `Update`, `UpdateType`, `DatasourceId` |
| `account.rs` / `account_utils.rs` | `AccountDecoder`, `DecodedAccount<T>`, account pipes |
| `account_deletion.rs` | Deletion pipes |
| `instruction.rs` | `InstructionDecoder`, `DecodedInstruction<T>`, `NestedInstruction(s)`, CPI re-nesting |
| `transaction.rs` | `TransactionMetadata`, transaction pipes |
| `collection.rs` | `InstructionDecoderCollection` |
| `filter.rs` | `Filter`, `FilterContext`, `FilterResult`, `DeduplicationFilter`, `DatasourceFilter` |
| `metrics.rs` | `Metric`, `MetricsExporter` |
| `transformers.rs` | Flat-instruction extraction, unnesting, metadata conversion |
| `deserialize.rs` | `CarbonDeserialize`, `ArrangeAccounts`, discriminator extraction |
| `block_details.rs` | Block-level metadata |
| `error.rs` | `Error` enum + `CarbonResult<T>` |
| `graphql/`, `postgres/` | Output-target support |

## Builder surface

```rust
Pipeline::builder()
    .datasource(d)                       // or .datasource_with_id(id, d)
    .account(decoder, processor)         // or .account_with_filters(..)
    .account_deletions(processor)        // or .account_deletions_with_filters(..)
    .instruction(decoder, processor)     // or .instruction_with_filters(..)
    .transaction(processor)              // NOTE: no schema argument in 1.0.0
    .metrics(Arc<dyn MetricsExporter>)
    .channel_buffer_size(1_000)
    .datasource_cancellation_token(token)
    .shutdown_strategy(ShutdownStrategy::Immediate)
    .build()?
    .run()
    .await?
```

The sniper uses exactly: `.datasource(YellowstoneGrpcGeyserClient)`, `.metrics(LogMetrics)`,
`.instruction(PumpfunDecoder, SniperProcessor)`, `.shutdown_strategy(Immediate)`.

## Properties that still hold from 0.6.x

- **Decoders short-circuit on identity first.** `instruction.program_id != PROGRAM_ID → None`;
  `account.owner != PROGRAM_ID → None`. Registering many decoders costs one comparison each per update.
- **Account decoding tries variants sequentially; first successful Borsh decode wins.** Declaration
  order in `accounts/mod.rs` is semantically load-bearing — two same-length account types mis-decode.
- **Instruction pipes recurse.** A processor fires for every match at every CPI depth, not just top
  level. Guard against double-counting.
- **CPI trees are rebuilt** from flat `(stack_height, instruction)` pairs — input order does not imply
  nesting.
- **A datasource whose `consume` errors is logged and dropped**; the pipeline keeps running with one
  fewer producer and surfaces no error. Monitor throughput, not just liveness.

### What changed about backpressure

0.6.x used an unbounded channel, so a fast producer could grow the queue until OOM. 1.0.0 uses a
bounded channel (default 1,000). That gives real backpressure — but a bounded channel means a producer
can now **block or drop** instead of buffering. The sniper deliberately sidesteps this by using its own
`mpsc::channel(1_024)` with `try_send`, so a full dispatcher queue drops the signal and logs rather
than stalling the geyser pipeline.

## Datasources (14)

| Crate | Transport |
|---|---|
| `yellowstone-grpc-datasource` | Yellowstone gRPC Geyser — **what the sniper uses** |
| `jito-shredstream-grpc-datasource` | Jito Shredstream — the unbuilt route to block 0 |
| `helius-laserstream-datasource` | Helius LaserStream |
| `helius-atlas-ws-datasource` | Helius Atlas WS |
| `helius-gpa-v2-datasource`, `helius-gtfa-datasource` | Helius GPA v2 / getTransactionsForAddress |
| `rpc-block-subscribe-datasource` | `blockSubscribe` |
| `rpc-program-subscribe-datasource` | `programSubscribe` |
| `rpc-block-crawler-datasource` | Historical block crawl |
| `rpc-transaction-crawler-datasource` | Historical tx crawl |
| `rpc-gpa-datasource` | `getProgramAccounts` |
| `jetstreamer-datasource` | Jetstreamer |
| `stream-message-datasource` | Generic stream messages |
| `validator-snapshot-datasource` | Validator snapshots |

## Metrics (2)

`log-metrics` (terminal) and `prometheus-metrics` (scrape endpoint), both implementing
`MetricsExporter`. The sniper uses `LogMetrics` only.

## Decoders (63)

Generated per-program crates. Structure has grown since 0.6.x:

```
decoders/<program>-decoder/src/
├── lib.rs           # Decoder struct + PROGRAM_ID
├── accounts/        # account types + AccountDecoder impl
├── instructions/    # instruction types + InstructionDecoder impl
├── events/          # NEW in 1.0.0 — CPI event types
├── types/           # shared IDL types
└── graphql/         # NEW — GraphQL schema types
```

Individual instruction dirs may also carry `postgres/` row types.

**Never hand-edit `decoders/`** — it is generated output. Regenerate via the CLI; to change output
shape, edit the renderer.

## CLI moved to TypeScript

The codegen CLI is no longer `crates/cli`. It now lives under `packages/`:

| Package | Role |
|---|---|
| `packages/cli` | The `carbon` CLI |
| `packages/renderer` | IDL → Rust decoder rendering (Codama + legacy Anchor) |
| `packages/versions` | Version management |

The repo is therefore a **dual monorepo**: Cargo workspace (`crates/*`, `datasources/*`, `decoders/*`,
`examples/*`, `metrics/*`) plus pnpm + turbo (`pnpm-workspace.yaml`, `turbo.json`,
`tsconfig.base.json`). Note `ws/` and the old `swap/` service are gone, and `crates/cli` no longer
exists.

## Version note

`rust-toolchain.toml` pins **1.88.0**, while `Cargo.toml` declares `rust-version = "1.82"` as MSRV.
The pin is what builds; the MSRV is the compatibility claim. Both are correct and they differ on
purpose.
