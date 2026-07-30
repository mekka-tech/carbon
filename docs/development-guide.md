# Development Guide

Branch `sniper`. All commands from the repository root.

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | **1.88.0** | Pinned in `rust-toolchain.toml`; rustup installs it automatically on first `cargo` call |
| `protoc` | 3.21+ | Required — Yellowstone gRPC builds proto definitions |
| `pkg-config`, `libssl-dev`, `build-essential` | — | Native deps for `reqwest`/rustls |
| pnpm | — | Only for `packages/` (TS codegen); not needed to build the sniper |

Verified working on this machine: cargo/rustc 1.88.0, libprotoc 3.21.12, gcc 13.3.0.

## Build and test

```bash
cargo build -p pumpfun-sniper-example                 # debug
cargo build --release -p pumpfun-sniper-example       # what you deploy
cargo test  -p pumpfun-sniper-example                 # 36 tests
```

**Always scope with `-p`.** A bare `cargo build` compiles all 63 decoders and 14 datasources.

Expect ~4 minutes for a cold `cargo test` (pulls in solana-client, quic, streamer, yellowstone).

### Reading command results

Do **not** pipe cargo through `tail`/`head` and read the exit code — you get the pager's status, not
cargo's, and a hard failure looks like success. Use:

```bash
cargo test -p pumpfun-sniper-example 2>&1 | tail -40; echo "EXIT=${PIPESTATUS[0]}"
```

Similarly, `grep -c` exits 1 on zero matches, so `grep -c ... && next` silently skips `next`.

## Format and lint

```bash
./scripts/cargo-fmt.sh          # cargo fmt --all
./scripts/cargo-clippy.sh
```

The clippy script is stricter than default and **will reject code that compiles fine**:

```bash
cargo clippy --workspace --all-targets -- \
    --deny=warnings \
    --deny=clippy::default_trait_access \
    --deny=clippy::arithmetic_side_effects \
    --deny=clippy::manual_let_else \
    --deny=clippy::used_underscore_binding
```

`clippy::arithmetic_side_effects` is the one that bites: bare `+ - *` on integers is denied. Use
`checked_*` / `saturating_*` / `wrapping_*`. This matters throughout the sniper, which does lamport
and reserve arithmetic everywhere.

Note the script is `--workspace`, so it lints all 63 decoders too and takes a while.

## Running locally

You largely **cannot**. The sniper needs Yellowstone gRPC, which is HTTP/2 — a sandboxed session's
HTTP(S) egress proxy cannot carry it, nor raw TCP. Plain JSON-RPC over 443 does work, which is how the
mainnet verification in `VERIFICATION.md` was performed.

What you *can* do locally: build, test, clippy, and any JSON-RPC-only verification.

Anything requiring geyser — including `SEND_MODE=simulate` against live launches — must run on the
deployment box. See [Deployment Guide](./deployment-guide.md).

```bash
cp examples/pumpfun-sniper/.env.example .env    # then fill it in
RUST_LOG=info cargo run --release -p pumpfun-sniper-example
```

`RUST_LOG=info` gives the `SNIPE` lines and dispatch summaries. `RUST_LOG=debug` adds sender detail.

**Leave `SEND_MODE` unset while developing** — unset means dry run. See
[Configuration Reference](./configuration-reference.md) for why any other value is dangerous.

## Conventions

| Area | Convention |
|---|---|
| Imports | One braced `use { .. }` block per file — enforced by `rustfmt.toml` (`group_imports = "One"`, `imports_granularity = "One"`), not style preference |
| Errors | `CarbonResult<T>`; `Error::Custom(String)` for sniper-specific failures. Config returns `Result<_, String>` |
| Arithmetic | Checked/saturating — plain ops fail clippy |
| Comments | The sniper comments *why*, not *what* — non-obvious constants (QUIC pool=1, 400 ms timeout, tip placement) carry their rationale inline. Match this |
| Decoders | Generated. Never hand-edit `decoders/`; change `packages/renderer` instead |
| Commits | This branch uses conventional commits: `feat(sniper):`, `fix(sniper):`, `docs:`, `chore(sniper):`. The older branches did not — match this branch |
| `rustfmt.toml` | `comment_width = 80`, `wrap_comments = true` |
| `taplo.toml` | `reorder_keys = true` on dependency tables |

## Testing patterns

Tests are inline `#[cfg(test)] mod tests` in the module under test. The valuable ones assert against
**real mainnet fixtures** rather than synthetic data — `buy_reproduces_a_real_mainnet_instruction`
rebuilds a buy and diffs it account-for-account against a known-good transaction.

When touching `pump/instructions.rs` or `pump/pdas.rs`, add a fixture-based test. A layout change that
compiles and passes synthetic tests can still fail every live buy — that is exactly what happened with
the 16-vs-18 account bug.

## CI

`.github/workflows/` is upstream Carbon's. The last commit on this branch (`e0cad975`,
"green up CI — clippy, unused dep, markdown formatting") suggests CI does run on this branch, but
verify what it covers before relying on it — the sniper is an `examples/*` member, and upstream CI may
not build examples.

Run `./scripts/cargo-clippy.sh` locally before pushing regardless.

## Adding a send path

1. New module in `src/sender/`, exported from `sender/mod.rs`.
2. Add a variant to `SendPath` in `config.rs` and to `parse_send_paths`.
3. Construct it in `main.rs`, pass into `BuyDispatcher::new`.
4. Push a future onto `paths` in `dispatch()` under a `send_paths.contains(..)` guard.
5. If it needs a tip in the transaction, add it in `build_buy_tx` — tips are per-transaction, and the
   provider assignment must stay index-aligned with the surviving `txs` vector.
