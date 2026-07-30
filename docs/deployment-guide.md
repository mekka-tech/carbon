# Deployment Guide

## Why deployment is operator-driven

A sandboxed agent session reaches the internet through an HTTP(S) egress proxy that **cannot carry raw
TCP (SSH) or gRPC/HTTP-2**. Yellowstone geyser is gRPC, so the sniper cannot be *run* from such a
session at any network-access setting — that setting governs which domains are reachable, not which
protocols.

Plain JSON-RPC over 443 does work, which is how `VERIFICATION.md`'s mainnet checks were done.

**Consequence:** every step below needs a machine that can SSH to the box. Run
`examples/pumpfun-sniper/deploy.sh` from your own machine.

## Target box

Hetzner **EX44** (i5-13500, 64 GB, 2×512 GB NVMe) in **FSN1 (Falkenstein)**.

Chosen for locality: the data provider (constant-k / Kaldera) runs bare metal in **Frankfurt**, ~4–5 ms
away. Everything stays in one metro — Frankfurt geyser, Frankfurt RPC, Frankfurt Jito block engine,
Frankfurt fast-landing endpoints.

**Do not use Helsinki** — it adds ~25 ms to every snipe, which is most of the budget.

## Provisioning

`deploy.sh` automates this. It deliberately does **not** write a live `.env` or flip `SEND_MODE` off
simulate — both are manual by design.

```bash
SNIPER_HOST=root@1.2.3.4 ./examples/pumpfun-sniper/deploy.sh provision   # deps, rust, chrony, build+test
SNIPER_HOST=root@1.2.3.4 ./examples/pumpfun-sniper/deploy.sh install     # systemd unit (simulate)
SNIPER_HOST=root@1.2.3.4 ./examples/pumpfun-sniper/deploy.sh logs        # follow journal
```

`SNIPER_HOST=local` runs it on the box instead. Overridable: `REPO_URL`, `BRANCH`
(default `claude/pumpfun-coin-sniper-mvwiwh`), `DIR` (default `/root/carbon`).

What `provision` does: installs `build-essential pkg-config libssl-dev protobuf-compiler git curl
chrony`, enables chrony, installs rustup if absent, clones/updates the branch, runs
`cargo test -p pumpfun-sniper-example`, builds release, seeds `.env` from `.env.example`, prints
`chronyc tracking`.

### Manual equivalent

1. `installimage` → Ubuntu 24.04, keep the RAID1 default.
2. SSH keys only; `ufw default deny incoming && ufw allow ssh && ufw enable`.
3. **`apt install -y chrony`** and verify with `chronyc tracking`.
4. `apt install -y build-essential pkg-config libssl-dev protobuf-compiler`, install rustup,
   `cargo build --release -p pumpfun-sniper-example`.
5. `cp examples/pumpfun-sniper/.env.example .env` and fill it in.
6. Run under systemd with `Restart=always`.

### chrony is not optional

The freshness gate compares each create's `block_time` against the local clock
(`MAX_TX_AGE_MS`, default 3000). Clock drift **silently** kills snipes (clock ahead → everything looks
stale) or admits stale replays (clock behind). There is no error, just wrong behaviour. Verify:

```bash
chronyc tracking | head -3
```

## Go-live sequence

From `SNIPER_HANDOFF.md` §6. **Only step 1 is currently done.** Do not skip ahead.

| # | Step | Status |
|---|---|---|
| 1 | `cargo test -p pumpfun-sniper-example` passes | ✅ 36/36, verified 2026-07-29 |
| 2 | Run the `VERIFICATION.md` procedure **on the box**: fetch recent successful pump.fun buys, diff their account lists and signer/writable flags against ours | ❌ **highest-risk item** — if the layout is wrong, every buy fails |
| 3 | `SEND_MODE=simulate` against live geyser; confirm the `SNIPE` log fires and simulation returns OK on a real launch | ❌ |
| 4 | One live snipe: `BUY_AMOUNT_SOL=0.001`, `SEND_PATHS=rpc`. Verify the fill on-chain | ❌ |
| 5 | Fill in real tip accounts for each `FAST_PROVIDERS` entry from that provider's docs | ❌ **a wrong tip account is accepted and then silently never lands.** Helius Sender uses its own tip accounts, not Jito's |
| 6 | Only then scale size, wallet count, and enable `fast`/`jito`/`tpu` | ❌ |

## Before sending real money

- **`SEND_MODE` is the only thing between simulate and live**, and any value other than unset or
  `simulate` is live. Not a route selector. See [Configuration Reference](./configuration-reference.md).
- **Fund every wallet.** Startup aborts if any wallet cannot cover
  `BUY_AMOUNT_SOL + FUNDING_BUFFER_SOL` — but only when not in dry run.
- **`MAX_POSITIONS` is a lifetime budget**, not a concurrent cap. `sniped_mints` never shrinks, so the
  process stops sniping once hit and must be restarted.
- **There is no exit.** Every snipe is a one-way commitment; nothing sells. Size accordingly.
- **Set `PRIORITY_FEE_JITTER`.** It defaults to `0`, which makes all 30 wallets bid identically and
  lose the same tiebreak together. `.env.example` sets `500000`.

## Operational notes

- **Jito rate-limits 1 request/second per region per IP.** Six bundles at one endpoint means five
  `429`s — this is why bundles are dealt across six regions. Helius Sender reaches Jito without
  consuming that per-IP budget.
- **Unstaked QUIC senders are deprioritized** by validators exactly under the load a contested launch
  creates. Direct-TPU landing improves a lot with a staked identity or rented staked connections.
- **A datasource that dies is logged, not restarted.** The carbon pipeline keeps running with one fewer
  producer and surfaces no error. Monitor for absence of `SNIPE` lines, not just process liveness.
- **A full dispatcher queue drops signals.** `try_send` on a 1,024-slot channel logs an error and
  discards. Grep the journal for `failed to queue snipe signal`.
- **Re-run `VERIFICATION.md` after any pump.fun program upgrade.** Two of the 18 buy accounts are
  undocumented and can change with no compile error and no test failure.

## Not built

No latency probe, no metrics beyond `LogMetrics` (terminal logging), no alerting, no position
tracking. `carbon-prometheus-metrics` exists in the workspace and is not wired up.

## Suggested systemd unit

`deploy.sh install` writes one. Shape, for reference:

```ini
[Unit]
Description=pump.fun sniper
After=network-online.target chrony.service

[Service]
WorkingDirectory=/root/carbon
EnvironmentFile=/root/carbon/.env
Environment=RUST_LOG=info
ExecStart=/root/carbon/target/release/pumpfun-sniper-example
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Note `After=chrony.service` — starting before the clock is synced re-introduces the drift problem the
freshness gate is sensitive to.
