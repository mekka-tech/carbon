# 30-Wallet Simultaneous Buy Dispatch — Design

Goal: on a watched creator's coin creation, fire **30 independent buy
transactions from 30 different wallets**, landing in the **same block as the
create (block 0) or the very next block (block 1)**. Not one Jito bundle —
singular transactions sprayed across multiple send paths / providers.

## Decisions (locked)

- **Send path: RPC spray + direct TPU**, both in parallel. Multi-RPC for
  reliability, leader-targeted QUIC for latency.
- **Detection: geyser now** (reliable block-1 floor), **shredstream added in a
  later milestone** once constant-k/Jito shred access is confirmed.
- **Jito: optional parallel path**, off by default, config-enabled — 6 bundles
  of 5 with tips, fired alongside the spray (never instead of it).
- **Funding: pre-funded wallets**, batch balance preflight at startup that
  aborts/skips underfunded wallets. No auto-distribution in v1.

## Honest latency framing (read first)

Solana slots are ~400 ms. What "block 0 / block 1" actually costs:

- **Block 1 (next block) — reliably achievable.** Detect the create, build +
  sign 30 txs, and get them to the *next* leader's TPU with a competitive
  priority fee before that leader's slot ends. Total pipeline budget ~300 ms.
  This is the standard, dependable sniper target and what the design centers
  on.
- **Block 0 (same block as the create) — best-effort, and provider-dependent.**
  Requires seeing the create *before the producing leader finishes the block*
  and getting our tx back to that same leader in time — a window of tens of ms.

  The subscription already runs at **`CommitmentLevel::Processed`** (see
  `main.rs`), which is the precondition: `confirmed`/`finalized` would deliver
  the create only well after block N is sealed, making block 0 structurally
  impossible. At `processed`, whether block 0 is reachable comes down to *when
  the provider emits* — a geyser plugin that streams transaction updates as the
  validator executes them can surface the create mid-block, while one that
  flushes on block boundaries cannot. Providers differ, and some that advertise
  `processed` still emit per completed block.

  So: treat block 1 as the dependable floor and block 0 as upside that depends
  on the feed. Measure it rather than assuming either way — log the delta
  between the create's slot and the slot our fills land in, and compare feeds.
  **Shred-level detection** (Jito shredstream, or constant-k's shred injection
  / pre-execution fast path) removes the dependency entirely by reading partial
  shreds before the block is assembled, which is why it stays on the roadmap
  regardless.

So: detection latency is the whole game. The send path decides how many of the
30 land in block 1 vs slip to block 2+.

## 1. Detection (the front half)

Two selectable sources, same downstream `SnipeSignal`:

- **Geyser (Yellowstone, processed)** — current implementation. Reliable,
  simple, block-1 floor.
- **Shredstream** (`carbon-jito-shredstream-grpc-datasource`, already on main;
  constant-k may also expose a shred/fast-path) — sees the create tens of ms
  earlier, from partial shreds, before the block is finalized. This is the
  only path that opens a block-0 shot. Add as an alternate datasource behind a
  `DETECTION_SOURCE=geyser|shredstream` flag; the processor logic is identical.

Run **both concurrently** if available (shredstream for speed, geyser as the
authoritative backstop), deduping on mint so we never double-fire.

## 2. Build (30 txs, in parallel, minimal weight)

Per watched-creator create, build one `buy_exact_sol_in` tx per wallet:

- **`track_volume = false`** on every buy. This drops the
  `user_volume_accumulator` write (and its per-wallet rent + compute) — we want
  the leanest possible tx for a snipe, not volume rewards.
- **All accounts derived locally** (already done): no RPC on the hot path.
- **One warm blockhash** shared across all 30 (background refresher already in
  place). Optionally pre-warm to the *next* leader's expected blockhash.
- **Per-wallet priority-fee jitter.** Base `PRIORITY_FEE_MICRO_LAMPORTS` plus a
  deterministic per-index offset so the 30 txs occupy a *spread* of priority
  levels instead of tying at one price — more of them clear the block-1 cutoff,
  and ties (which the leader breaks arbitrarily) are avoided.
- **Per-wallet config** ("30 wallets differently"): each wallet can carry its
  own buy amount, priority fee, and (optionally) send-path assignment.
- Signing 30 ed25519 txs on the EX44 is sub-millisecond-to-low-ms; do it with
  a parallel map, not a serial loop.

## 3. Send — fanout across providers (the back half)

The core idea: **the same signed tx sent to many endpoints is idempotent
on-chain** (same signature ⇒ included at most once). So we duplicate each of
the 30 txs across every send path; whichever network route reaches the leader
first wins, with zero risk of double-buying.

Send matrix = **30 txs × M paths**, all fired concurrently (bounded fan-out):

1. **Multi-RPC spray.** `RPC_URLS` = comma-separated list (constant-k Frankfurt
   + 1–2 others). Each tx → every RPC via `sendTransaction`,
   `skipPreflight=true`, `maxRetries=0` (we manage retries; a preflight or
   retry would cost us the slot). Spreading across providers also sidesteps any
   single provider's per-second cap when bursting 30.
1b. **Fast / anti-MEV landing providers** (`FAST_PROVIDERS`, path `fast`).
   Tip-based landing services that sit closer to leaders than a normal RPC:
   Helius Sender (`fra-sender.helius-rpc.com/fast`, no API key, min 0.001 SOL
   or 0.000005 SOL with `?swqos_only=true`), Hello Moon Lunar Lander
   (`fra.lunar-lander.hellomoon.io/send`, min 0.001 SOL), Jito's single-tx
   endpoint, Nextblock/0slot/Temporal/Astralane, etc. They share one shape —
   normal transaction + a tip transfer to the provider's tip account, POSTed to
   a regional endpoint — so they're described in config, not hardcoded:
   `name|url|tip_accounts|tip_sol[|auth[|format]]`. Wallets are dealt
   round-robin across providers, and each wallet's tip is baked into its
   transaction at build time. All Frankfurt endpoints, matching the FSN1 box.
   **Tip accounts and body formats must be taken from each provider's docs** —
   a wrong tip account silently loses the transaction.
2. **Direct TPU to the next leader(s)** *(highest-value latency path)*. Use the
   leader schedule + `solana-tpu-client`/QUIC to push each tx straight to the
   TPU of the next 1–2 leaders, bypassing RPC entirely. Keep QUIC connections
   to upcoming leaders pre-warmed so the send is a single round trip. This is
   what makes block-1 consistent and gives block-0 its only realistic shot.
3. **Jito (optional path, not the primary).** If enabled, also submit as 6
   bundles of 5 with a tip on each bundle's last tx — useful only when the next
   leader is a Jito leader (~half of slots). Runs *in parallel* with the spray,
   never instead of it. Off by default per your preference.

Partial fills are expected and fine: with 30 independent txs some land in
block 1, some block 2, some fail on slippage. That's the point of spraying —
maximize count in the earliest blocks; use bundles only if you needed
all-or-nothing (you don't).

## 4. Wallets & funding (30 keypairs)

- Load 30 keypairs (`BUYER_KEYPAIRS`, or a keypair directory).
- **Startup preflight:** fetch all 30 balances in one batch; each wallet needs
  `buy_amount + priority/base fees + ATA rent (~0.002 SOL) + creator_vault /
  (optional) volume-accumulator rent on first buy`. Abort (or warn + skip
  underfunded wallets) before going live — a silently underfunded wallet is a
  dead buy.
- Optional `fund`/`drain` helper subcommand to distribute from / sweep back to
  a treasury wallet, so you don't hand-fund 30 addresses. Separable; phase-b.

## 5. Config additions (on top of current `.env`)

```
DETECTION_SOURCE=geyser            # geyser | shredstream | both
SHREDSTREAM_URL=...                # if shredstream/both
RPC_URLS=https://a,https://b       # multi-provider spray (replaces RPC_URL)
SEND_PATHS=rpc,tpu                 # any of: rpc, tpu, jito
TPU_LEADERS_AHEAD=2                # how many upcoming leaders to target
BUYER_KEYPAIRS=...                 # 30 keypairs (or BUYER_KEYPAIR_DIR)
BUY_AMOUNT_SOL=0.05                # default; per-wallet override optional
PRIORITY_FEE_MICRO_LAMPORTS=2000000
PRIORITY_FEE_JITTER=500000         # per-wallet spread
TRACK_VOLUME=false
MAX_TX_AGE_MS=...                  # tighter with shredstream
```

## 6. Module changes

- `sender/` gains `tpu.rs` (leader-targeted QUIC sender, pre-warmed) and
  `rpc.rs` grows to a multi-endpoint pool; `mod.rs` exposes a `SendPath` set
  the dispatcher fans across.
- `dispatch.rs`: build 30 txs in parallel, then a bounded concurrent
  `tx × path` fan-out instead of the current per-buyer loop.
- new `datasource.rs`: geyser | shredstream | both selection.
- `config.rs`: the fields above + 30-keypair loading + funding preflight.
- `wallets.rs`: keypair loading, balance preflight, per-wallet params.

## 7. Risks / limits

- **Provider rate limits.** 30 × M sends in a burst — spreading across
  providers and TPU is the mitigation; log any 429s so we can tune.
- **Blockhash expiry** is a non-issue at this timescale (150 slots ≈ 60 s) but
  the refresher must never stall the send path (it doesn't — separate task).
- **QUIC/TPU staking.** Unstaked senders get lower TPU priority; direct-TPU
  landing improves markedly if the sending identity has some stake or you use a
  staked-connection provider. Note for later tuning.
- **Compute budget.** Keep CU limit tight (~90–120k) so priority-fee-per-CU
  bids stay cheap while still competitive.

## 8. Milestones

1. Multi-wallet build + parallel `tx × multi-RPC` fan-out, `track_volume=false`,
   priority-fee jitter, funding preflight. (block-1 via RPC spray)
2. Direct-TPU sender with pre-warmed leader connections. (consistent block-1,
   block-0 shot)
3. Shredstream detection path + dedupe with geyser. (block-0 best-effort)
4. Jito optional path; funding/drain helper.
