# Pump.fun Sniper — Handoff / Resume Guide

Everything needed to pick this work up in a fresh session or on the
deployment box. Written because this session could not push (see
[Blocked: repo access](#blocked-repo-access)).

Branch: `claude/pumpfun-coin-sniper-mvwiwh` (based on `main` @ `af70b199`)

---

## 1. What this is

A Rust-only pump.fun sniper: watch a list of creator wallets over Yellowstone
gRPC, and the instant one of them creates a coin, fire **30 independent buy
transactions from 30 different wallets** across multiple send routes, aiming to
land in the same block as the create ("block 0") or the next one ("block 1").

It replaces three outdated branches (`sniper-all-tokens`, `test/1`,
`feature/ben`) which used a Rust listener bridged over a WebSocket to a
TypeScript swap service. Those branches share **no merge base** with current
`main` and their hand-rolled buy transactions no longer work on-chain — see
`SNIPER_PLAN.md` for the full analysis.

## 2. Read these, in order

| Doc | What's in it |
|---|---|
| `SNIPER_PLAN.md` (repo root) | Architecture, why the old branches are dead, milestones |
| `examples/pumpfun-sniper/DISPATCH_PLAN.md` | The 30-wallet fanout design + locked decisions |
| `examples/pumpfun-sniper/PROVIDERS.md` | Verified endpoints, tip accounts, minimums, rate limits |
| `examples/pumpfun-sniper/VERIFICATION.md` | What's verified vs assumed about the buy instruction |
| `examples/pumpfun-sniper/README.md` | How to run it |
| `examples/pumpfun-sniper/.env.example` | Every config knob, documented |

## 3. Current state

### Working and tested
- **Listener** — carbon pipeline on pump.fun `Create`, filtered to watched
  creator wallets, with the legacy bot's guards ported: creator blacklist,
  min creator balance, max creator dev-buy, per-mint dedup, max open
  positions, transaction-age gate. Decodes the creator's dev buy out of the
  create transaction to price the curve shift.
- **Buy builder** — `buy_exact_sol_in` with all 18 accounts derived locally
  (creator vault, volume accumulators, fee config), so there are **zero RPC
  calls on the hot path**. Instruction data reuses the decoder's borsh structs
  rather than hardcoded bytes.
- **Quote math** — the program's own documented formula, shifted by the dev
  buy, reduced by slippage. Unit tested.
- **Dispatch** — 30 transactions built and signed in parallel, then fanned
  concurrently across every enabled send path.
- **Send paths** — `rpc` (spray to every endpoint in `RPC_URLS`), `fast`
  (configurable anti-MEV landing providers), `jito` (bundles of 5 dealt across
  regions), `simulate` (dry run).
- **Safety** — startup balance preflight refuses to go live with underfunded
  wallets; `Global` and `fee_config` are checked on-chain at boot so a wrong
  PDA derivation fails fast instead of silently losing buys.

- **Direct-TPU send path** (`src/sender/tpu.rs`) — tracks the leader schedule,
  resolves each upcoming leader's TPU/QUIC socket from gossip, and keeps those
  QUIC connections pre-warmed so a send is one round trip. Refreshed every
  400 ms in the background; every failure degrades to "log and keep the
  previous target set". 11 unit tests cover the leader arithmetic and socket
  resolution. Enable with `SEND_PATHS=…,tpu`.

  Two constants worth knowing about if it misbehaves: the connection pool is
  pinned to **1** (the cache picks a random pool member per send, so a larger
  pool could hand you an unwarmed connection), and there's a per-leader
  **400 ms send timeout** (QUIC gives no application ack, so a black-holed peer
  would otherwise stall the dispatcher past the slot being raced for).

- **Buy instruction verified against mainnet.** The audit pulled pump.fun's
  on-chain Anchor IDL and 49 real successful buys, then used differential
  `simulateTransaction` to prove the exact layout. It found a **critical bug**:
  the builder passed 16 accounts, but the deployed program requires **18** —
  every live buy would have failed with `BuybackFeeRecipientMissing` (6062).
  The two extra accounts come from `remaining_accounts` and appear in neither
  the IDL nor the Codama decoder:
  - `remaining[0]` = `bonding_curve_v2`, PDA `["bonding-curve-v2", mint]`,
    read-only. Wrong address => `InvalidBondingCurveV2` (6074).
  - `remaining[1]` = one of `Global.buyback_fee_recipients`, **writable**.
    Read-only => `PrivilegeEscalation`; outside the set =>
    `BuybackFeeRecipientNotAuthorized` (6057).

  Also fixed: `global_volume_accumulator` was writable (IDL says read-only, and
  a write lock there serialises us against every other pump buyer in the slot),
  and the token program is now a field rather than a hardcode. See
  `VERIFICATION.md` for the evidence and transaction signatures.

### In flight when this session ended
Nothing — both subagents completed and their work is committed.

- **`create_v2` launches are sniped too.** `CreateV2` now produces a snipe
  signal like `Create`, and the dispatcher builds `buy_exact_quote_in_v2`
  (27 accounts) instead of `buy_exact_sol_in`. v2 coins are Token-2022 and
  trade against a *quote mint* (WSOL), so each v2 buy also wraps SOL —
  create WSOL ATA → transfer → `sync_native` → create the Token-2022 ATA → buy,
  with an optional `close_account` to unwrap the remainder.

  Verified against mainnet the same way v1 was: all 27 derived addresses
  matched three real successful buys account-for-account. Unlike v1, the
  trailing `bonding_curve_v2` is **optional** here — v2 promotes
  `buyback_fee_recipient` to a named account, so a 27-account buy is complete.
  It is appended only for `is_mayhem_mode` coins, on the mayhem program.
  See VERIFICATION.md §5. Toggle with `SNIPE_V2` (default on).

  v2 was ~21% of live buy traffic in the sampled blocks (11 of 52), so this was
  not a marginal path.

### Not built yet
- **Shredstream detection** (`M3`) — the only route to block 0. Geyser at
  processed commitment delivers a create *after* its block is built, so block 1
  is the floor with geyser alone. `carbon-jito-shredstream-grpc-datasource` is
  already in the workspace.
- **`buy_v2`** (exact-tokens-out) — only the exact-quote-in variant is built,
  which is what a sniper wants. Add if a fixed token count is ever needed.
- **Differential simulation for v2** — VERIFICATION.md §3.3 was run for v1 but
  not repeated for v2; the v2 layout rests on IDL + three reproduced mainnet
  buys. Worth running on the box before scaling v2 size.
- **Exit tracker (phase 2)** — no position tracking or auto-sell. Port the old
  branches' order book onto `CpiEvent::TradeEvent`.
- **Ops** — systemd unit, latency probe, metrics.

## 4. Blocked: repo access

`git push` fails with **403**, and so does the GitHub API with the session's
provisioned `GITHUB_TOKEN`:

```
$ curl -H "Authorization: Bearer $GITHUB_TOKEN" https://api.github.com/repos/mekka-tech/carbon
403
```

This is a genuine authorization restriction — the Claude GitHub App is not
enabled for `mekka-tech/carbon` — not a network problem (github.com itself
returns 200). **No PR could be created.**

To fix: an admin grants the Claude GitHub App access to the repo (GitHub →
Settings → Applications → Claude, or the Claude admin settings page). Once
granted, `git push -u origin claude/pumpfun-coin-sniper-mvwiwh` should work and
a PR can be opened with the body in section 7.

Until then the commits exist **only** in the session container and in the git
bundle that was sent to the user. Restore with:

```sh
git clone pumpfun-sniper.bundle -b claude/pumpfun-coin-sniper-mvwiwh
# or into an existing clone:
git fetch /path/to/pumpfun-sniper.bundle \
  claude/pumpfun-coin-sniper-mvwiwh:claude/pumpfun-coin-sniper-mvwiwh
```

## 5. Deployment target

Hetzner **EX44** (i5-13500, 64 GB, 2×512 GB NVMe) in **FSN1 (Falkenstein)**.
Chosen because the data provider (constant-k / Kaldera) runs bare metal in
**Frankfurt**, ~4–5 ms away. Everything stays in one metro: Frankfurt geyser,
Frankfurt RPC, Frankfurt Jito block engine, Frankfurt fast-landing endpoints.
Do **not** use Helsinki — it adds ~25 ms to every snipe.

Setup, in order:
1. `installimage` → Ubuntu 24.04, keep the RAID1 default.
2. SSH keys only; `ufw default deny incoming && ufw allow ssh && ufw enable`.
3. **`apt install -y chrony`** — not cosmetic: the freshness gate compares the
   create's `block_time` against the local clock, so drift silently kills or
   admits snipes. Verify with `chronyc tracking`.
4. `apt install -y build-essential pkg-config libssl-dev protobuf-compiler`,
   install rustup, `cargo build --release -p pumpfun-sniper-example`.
5. `cp examples/pumpfun-sniper/.env.example .env`, fill it in.
6. Run as a systemd unit with `Restart=always` (see README).

## 6. Before sending real money

1. `cargo test -p pumpfun-sniper-example` passes.
2. Run the `VERIFICATION.md` procedure **on the box** (it has RPC access):
   fetch recent successful pump.fun buys and diff their account lists and
   signer/writable flags against ours. This is the single highest-risk item —
   if the account layout is wrong, every buy fails.
3. Run with `SEND_MODE=simulate` against the live geyser and confirm the SNIPE
   log fires and simulation returns OK on a real launch.
4. One live snipe at `BUY_AMOUNT_SOL=0.001` via `SEND_PATHS=rpc`. Verify the
   fill on-chain.
5. Fill in real tip accounts for each `FAST_PROVIDERS` entry from that
   provider's docs — **a wrong tip account is accepted and then silently never
   lands**. Helius Sender uses its own tip accounts, *not* Jito's.
6. Only then scale up size, wallet count, and enable `fast`/`jito`/`tpu`.

## 7. PR body (paste when access is granted)

> ### Pump.fun creator-wallet sniper
>
> Rust-only rebuild of the legacy sniper branches on current `carbon-core`,
> replacing the Rust→WebSocket→TypeScript architecture with a single binary.
>
> **Why a rebuild rather than a rebase:** `sniper-all-tokens`, `test/1` and
> `feature/ben` share no merge base with `main`, and pump.fun has changed
> substantially since — `Buy` now needs 16 accounts (creator vault, volume
> accumulators, fee config) versus the 12 the old TypeScript builder sent, and
> there is a whole v2 quote-mint flow. The old transactions would fail on-chain.
>
> **What's here:**
> - `examples/pumpfun-sniper/` — new example crate
> - Listener on `Create` filtered to watched creator wallets, with balance,
>   blacklist, position and freshness guards
> - `buy_exact_sol_in` builder with locally-derived PDAs (no RPC on the hot
>   path) and the program's documented quote formula (unit tested)
> - 30-wallet parallel build/sign, fanned across RPC spray, configurable
>   anti-MEV landing providers, and Jito bundles dealt across regions
> - Startup balance preflight and on-chain PDA validation that fail fast
> - Docs: architecture, dispatch design, verified provider reference
>
> Defaults to `SEND_MODE=simulate` (dry run). Nothing sends until explicitly
> configured.

## 8. Known residual risks

- **The two trailing buy accounts are undocumented.** They are not in the IDL,
  so a program upgrade can change them with no IDL signal and no compile error
  — buys would simply start failing. Re-run the `VERIFICATION.md` procedure
  after any pump.fun upgrade.
- **`create_v2` / mayhem coins are unsupported.** `buy_v2` takes 27 accounts and
  derives `bonding_curve_v2` on the mayhem program instead.
- **Unstaked QUIC** (direct-TPU path) is deprioritized by validators exactly
  under the load a contested launch creates.

## 9. Gotchas worth remembering

- **Jito rate-limits to 1 request/second per IP per region.** Six bundles at
  one endpoint means five `429`s — bundles are dealt across regions for this
  reason. Helius Sender reaches Jito without consuming that per-IP budget.
- **Nozomi is not JSON-RPC.** Its `sendTransaction2` takes a bare base64 body
  as `text/plain` and returns no signature (`format=raw` in `FAST_PROVIDERS`).
- **A signature lands at most once**, which is why the same transaction can be
  sprayed down every route for free, and why a tip only costs anything if that
  particular transaction is the one that lands.
- **A Jito bundle caps at 5 transactions** — 30 buys can never be one bundle.
- **Unstaked QUIC senders are deprioritized** by validators; direct-TPU landing
  improves a lot with a staked identity or rented staked connections.
