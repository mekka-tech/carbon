# Fast / Anti-MEV Landing Providers

Reference for filling in `FAST_PROVIDERS` in `.env`. All of these follow the
same shape the sniper implements: a normal transaction that includes a SOL
transfer to the provider's tip account, POSTed to a regional endpoint.

**Frankfurt endpoints are listed first for every provider** — the box is in
Hetzner FSN1, ~4–5 ms from Frankfurt.

> Verify tip accounts and minimum tips against each provider's own docs before
> going live. A wrong tip account means the transaction is accepted and then
> silently never lands. Several of these docs sites block automated fetches, so
> the entries below marked "from docs" were confirmed and the rest need you to
> paste the current values.

## Verified

### Jito
- **Frankfurt:** `https://frankfurt.mainnet.block-engine.jito.wtf`
  - single tx: `/api/v1/transactions` (add `?bundleOnly=true` for revert protection)
  - bundles: `/api/v1/bundles` (max 5 txs)
- Other regions: `amsterdam`, `dublin`, `london`, `ny`, `slc`, `singapore`, `tokyo`
- **Minimum tip:** 1000 lamports (competitive launches need far more)
- **No API key required**
- ⚠️ **Rate limit: 1 request/second per IP per region.** This is the big one for
  a 30-buy fanout — six bundles at one endpoint means five `429`s. The sniper
  deals bundles round-robin across `JITO_BLOCK_ENGINE_URLS` for this reason.
- **Tip accounts** (all 8, hardcoded in `sender/jito.rs`, verified against
  Jito's docs):
  ```
  96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5
  HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe
  Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY
  ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49
  DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh
  ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt
  DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL
  3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT
  ```

### Helius Sender
- **Frankfurt:** `https://fra-sender.helius-rpc.com/fast`
- Other regions: AMS, TYO, SG, LAX, LON, EWR, PITT, SLC
- **No API key**, no credits billed
- **Minimum tip:** 0.001 SOL for full routing; 0.000005 SOL with
  `?swqos_only=true` (fewer pathways, no priority buffer)
- Submits in parallel to Jito **and** Helius (and Harmonic, Rakurai, …), so it
  is effectively several routes in one call — and it is not subject to Jito's
  per-IP limit the way a direct Jito call is.
- Uses Jito's tip accounts. Requires a priority fee *as well as* the tip.

### Hello Moon — Lunar Lander
- **Frankfurt:** `http://fra.lunar-lander.hellomoon.io/send`
- Others: `ams`, `nyc`, `ash`, plus geo-routed `lunar-lander.hellomoon.io/send`
- **Minimum tip:** 0.001 SOL to their tip account
- Runs on the same network as their nodes; supports QUIC, bundles, and
  multi-tx submission
- ⚠️ Tip account and exact request body: **get from
  https://docs.hellomoon.io/reference/lunar-lander** (the docs blocked
  automated fetch). If their `/send` takes a bare `{"transaction": "..."}`
  rather than JSON-RPC, set the 6th config field to `transaction`.

### Temporal / Nozomi
- Regions: **Frankfurt**, Amsterdam, US East
- Drop-in `sendTransaction` replacement; API key goes in the URL as `?c=<uuid>`
- **Minimum tip:** 0.001 SOL to the Nozomi tip address
- Higher tip = higher queue priority

## Require an account / API key

Same pattern, keys needed — worth adding once the core paths are proven:

| Provider | Notes |
|---|---|
| **bloXroute** Trader API | Tip transfer to an official bloXroute tipping address; rotate across their tip wallets; QUIC supported; private tip wallets for high-volume users |
| **0slot / ZeroSlot** | `ZERO_SLOT_KEY` |
| **NextBlock** | `NEXTBLOCK_API_KEY` |
| **BlockRazor** | `BLOCKRAZOR_API_KEY` |
| **Astralane** | `ASTRALANE_API_KEY` |

## Practical notes for the 30-buy fanout

- **Helius Sender is the best single addition**: no API key, Frankfurt, and it
  fans out to Jito + Helius + others internally, so one call buys several
  routes without inheriting Jito's 1 req/s/IP/region cap.
- **Don't point all 30 wallets at one provider.** The sniper deals wallets
  round-robin across `FAST_PROVIDERS` precisely so that per-provider rate
  limits and queue positions are spread. Three providers × 10 wallets is a
  sane starting shape.
- **Tips are per-transaction and only paid if that transaction lands.** With 30
  buys at 0.001 SOL, worst case (all 30 land) is 0.03 SOL of tips — budget it,
  but it is not 30× on every launch since only the ones that land pay.
- **Every tipped transaction still goes out over plain RPC too.** A signature
  lands at most once, so the extra routes are free insurance.
- Jito's own docs note the 1000-lamport minimum is *the floor, not a
  competitive bid* — for contested launches the tip is the bid.
