# Configuration Reference

Every environment variable, read from `examples/pumpfun-sniper/src/config.rs`. Defaults below are
what the **code** does, which is not always what `.env.example` shows.

## ⚠ `SEND_MODE` — read this first

```rust
let dry_run = matches!(env::var("SEND_MODE").unwrap_or_default().as_str(), "" | "simulate");
```

| Value | Effect |
|---|---|
| *unset* | **dry run** (safe) |
| `simulate` | **dry run** (safe) |
| anything else — `live`, `rpc`, `jito`, a typo | **SENDS REAL TRANSACTIONS** |

`SEND_MODE` does **not** select a send route. Routes come from `SEND_PATHS`. The branch README
incorrectly presents `SEND_MODE=rpc` as a route choice — that goes live.

## Feed selection

| Variable | Default | Effect |
|---|---|---|
| `DATASOURCE` | `yellowstone` | `yellowstone` (aliases `geyser`) or `shredstream` (aliases `shreds`, `prism`). Anything else is a fatal config error. |

The two feeds are not interchangeable — see the trade-off table in
[Verification Status](./verification-status.md#shredstream-feed--added-2026-07-29).

| | Yellowstone | Shredstream |
|---|---|---|
| Metadata | real | **fabricated** — no balances, status always `Ok(())`, `block_time` = local receive time |
| Filtering | server-side, pump program only | **none** — every network transaction decoded locally |
| Confirmation | confirmed | **pre-confirmation** — a create seen may never land |
| Earliest block | block 1 | **block 0** |
| Guards active | all 9 | 6 — balance and freshness guards are skipped |

## Required

| Variable | Notes |
|---|---|
| `GEYSER_URL` | Yellowstone gRPC endpoint. **Fatal if unset when `DATASOURCE=yellowstone`.** |
| `SHREDSTREAM_URL` | Shredstream proxy. **Fatal if unset when `DATASOURCE=shredstream`.** Must be `http://` or `https://` — **tonic rejects a `grpc://` scheme**, so rewrite `grpc://host:port` as `https://host:port`. Config errors with that explanation rather than failing obscurely at connect time. |
| `SHREDSTREAM_X_TOKEN` | `x-token` metadata for the proxy; falls back to `X_TOKEN`. Hosted proxies **require** it — without it you get a bare `HTTP 204`, surfacing as `malformed header: missing HTTP content-type`, not an auth error. Absence is warned about at startup. |
| `RPC_URLS` | Comma-separated. First is primary (used for `Global`, balances, blockhash, leader schedule). **Fatal if unset.** |
| `WATCHED_CREATORS` | Comma-separated pubkeys. Empty means nothing is ever sniped. |
| Buyer keypairs | Via `BUYER_KEYPAIR_DIR`. **Fatal if none load.** |

### Guards refused or disabled on shredstream

| Variable | On shredstream |
|---|---|
| `MIN_CREATOR_BALANCE_SOL` > 0 | **Hard error at startup.** With no balances `pre` reads 0, so `0 < floor` is false and every launch would be admitted while you believed a floor was enforced. |
| `MAX_CREATOR_BUY_SOL` | Guard inactive (startup warning). Value still used as the dev-buy fallback. |
| `MAX_TX_AGE_MS` | Freshness gate inactive (startup warning) — every create measures ~0 ms old. |

## Buyer wallets

| Variable | Default | Notes |
|---|---|---|
| `BUYER_KEYPAIR_DIR` | — | Directory of keypair files. Every wallet buys `BUY_AMOUNT_SOL`. |
| `BUY_AMOUNT_SOL` | `0.01` | Per wallet, per snipe. 30 wallets × 0.01 = 0.3 SOL committed per launch. |
| `FUNDING_BUFFER_SOL` | `0.01` | Headroom required above buy amount in the startup preflight. |

**Startup aborts** if any wallet cannot cover `BUY_AMOUNT_SOL + FUNDING_BUFFER_SOL`, unless in dry run.

## Targeting and guards

| Variable | Default | Effect |
|---|---|---|
| `BLACKLISTED_CREATORS` | empty | Skip these creators outright. |
| `MIN_CREATOR_BALANCE_SOL` | `0` | Skip if the create's fee payer held less pre-tx. |
| `MAX_CREATOR_BUY_SOL` | `5` | Skip if the creator spent more in the create tx. Also the fallback dev-buy estimate. |
| `MAX_POSITIONS` | `5` | **Lifetime** cap, not concurrent — `sniped_mints` never shrinks. |
| `MAX_TX_AGE_MS` | `3000` | Skip creates older than this vs the local clock. **Requires `chrony`.** |

## Launch flows

| Variable | Default | Effect |
|---|---|---|
| `SNIPE_V2` | **`true`** | Snipe `create_v2` (Token-2022, quote-mint) launches. Contrary to `README.md` and `SNIPER_HANDOFF.md` §8, this **is** implemented. |
| `UNWRAP_AFTER_BUY` | `true` | Append `close_account` on the WSOL ATA after a v2 buy to reclaim unspent quote + rent. |
| `TRACK_VOLUME` | `false` | Sets the `track_volume` flag on v1 buys. Note: only `"true"` enables it (`is_ok_and(|v| v == "true")`), unlike other bools which accept `=false` to disable. |

## Execution

| Variable | Default | Effect |
|---|---|---|
| `SEND_PATHS` | `rpc` | Comma-separated subset of `rpc,fast,jito,tpu`. |
| `SLIPPAGE_BPS` | `500` (5%) | Reduces `min_tokens_out`. |
| `COMPUTE_UNIT_LIMIT` | `120000` | |
| `PRIORITY_FEE_MICRO_LAMPORTS` | `100000` | Base of the per-wallet ladder. |
| `PRIORITY_FEE_JITTER` | `0` | Ladder span: wallet *i* pays `base + jitter*i/(n-1)`. |

> With the default `PRIORITY_FEE_JITTER=0`, all 30 wallets bid **identically** and lose the same
> tiebreak together. `.env.example` sets `500000`; the code default does not.

## Fast providers

`FAST_PROVIDERS` is a comma-separated list of pipe-delimited entries:

```
name|url|tipAccount1+tipAccount2|tipSol
```

`SEND_PATHS` including `fast` with an empty `FAST_PROVIDERS` is a **fatal config error**. Wallets are
dealt round-robin across providers; each wallet's tip is baked into its own transaction.

`PayloadFormat` supports JSON-RPC and `raw` (bare base64, `text/plain`) — the latter for Nozomi's
`sendTransaction2`, which returns no signature.

> **A wrong tip account is accepted by the provider and then silently never lands.** Helius Sender
> uses its own tip accounts, not Jito's. Pull each from that provider's docs — see `PROVIDERS.md`.

## Jito

| Variable | Default | Effect |
|---|---|---|
| `JITO_BLOCK_ENGINE_URLS` | Frankfurt, Amsterdam, London, Dublin, NY, SLC | Nearest-first. |
| `JITO_TIP_SOL` | **`0.0001`** | `.env.example` shows `0.001` — 10× the code default. |

Bundles cap at 5 transactions, so 30 buys become 6 bundles. Jito rate-limits **1 request/second per
region per IP**, which is precisely why bundles are dealt across 6 regions rather than sent to one.

## TPU

| Variable | Default | Effect |
|---|---|---|
| `TPU_LEADERS_AHEAD` | `2` | Upcoming leaders to resolve and keep warm. `0` disables the path (logs a warning if `SEND_PATHS` includes `tpu`). |

## Geyser

| Variable | Default | Effect |
|---|---|---|
| `X_TOKEN` | none | Optional gRPC auth token. |

Commitment is hardcoded to `Processed`, and the transaction filter is
`account_include = [pump program]`, `vote: false`, `failed: false`.

## Code-vs-example divergences

`.env.example` is a reasonable starting profile, not a mirror of the defaults:

| Variable | Code default | `.env.example` |
|---|---|---|
| `PRIORITY_FEE_MICRO_LAMPORTS` | `100000` | `1000000` |
| `PRIORITY_FEE_JITTER` | `0` | `500000` |
| `JITO_TIP_SOL` | `0.0001` | `0.001` |
| `SEND_PATHS` | `rpc` | `rpc,fast,tpu` |
| `MIN_CREATOR_BALANCE_SOL` | `0` | `0` |

`.env.example` also ships a **real-looking Helius Frankfurt endpoint and tip accounts**. Verify them
against current Helius docs before trusting them — a stale tip account fails silently.

## Minimum safe first run

```bash
GEYSER_URL=...
RPC_URLS=https://your-rpc
WATCHED_CREATORS=<one pubkey>
BUYER_KEYPAIR_DIR=/root/carbon/wallets
BUY_AMOUNT_SOL=0.001
MAX_POSITIONS=1
# SEND_MODE deliberately unset → dry run
SEND_PATHS=rpc
```

Then confirm a `SNIPE` log line fires and simulation returns OK on a real launch **before** setting
`SEND_MODE=live`.
