# Pump.fun Creator-Wallet Sniper

Listens for pump.fun coin creation by watched creator wallets over Yellowstone
gRPC and dispatches buys from multiple wallets. Rust-only successor to the
legacy `sniper-all-tokens` branch (Rust listener + TypeScript swap service),
rebuilt on the current carbon core and pumpfun decoder. See `SNIPER_PLAN.md`
at the repo root for the full background and design.

## How it works

1. A carbon pipeline subscribes to pump.fun transactions at processed
   commitment and decodes them with `carbon-pumpfun-decoder`.
2. `SniperProcessor` matches `Create` instructions from watched creators and
   applies the legacy bot's guards: creator blacklist, minimum creator
   balance, maximum creator dev-buy, per-mint dedup, max open positions, and
   a transaction-age gate.
3. A `SnipeSignal` goes over an in-process channel to `BuyDispatcher`, which
   builds one `buy_exact_sol_in` transaction per buyer keypair. All accounts
   (creator vault, volume accumulators, fee config) are derived locally —
   no RPC on the hot path; the blockhash is kept warm by a background task.
4. Transactions are sent per `SEND_MODE`: `simulate` (dry-run), `rpc`
   (independent sends, skip preflight), or `jito` (one atomic bundle, max 5
   buyers, tip on the last transaction).

The quote uses the official `buy_exact_sol_in` formula with initial virtual
reserves from the `Global` account, shifted by the creator's dev buy (decoded
from the create transaction) and reduced by `SLIPPAGE_BPS`.

## Running

```sh
cp .env.example .env   # fill in endpoints, creators, keypairs
RUST_LOG=info cargo run --release -p pumpfun-sniper-example
```

Start with `SEND_MODE=simulate` and check the simulation logs before going
live. At startup the sniper fetches `Global`, and fails fast if the derived
`fee_config` PDA doesn't exist on-chain (i.e. the fee program moved).

## Not yet implemented

- `create_v2` coins (quote-mint flow): logged and skipped; buys use the v1
  instruction only.
- Exits: no position tracking or auto-sell yet (phase 2 in the plan).
