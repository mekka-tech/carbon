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

## What we already run in production

The `dumpfun-frontend` repo (`lib/transaction-services/`) uses three services,
one adapter each — **Jito Block Engine**, **Nozomi (Temporal)**, and a
**fast RPC** adapter — behind a factory/selector with a region-aware
endpoint manager. All three are covered below, so the sniper aligns with the
stack that is already proven in production rather than introducing new
providers.

Two deliberate differences in the Rust sniper:

- **One generic implementation instead of a class per adapter.** Providers are
  described in config (`name|url|tip_accounts|tip_sol[|auth[|format]]`), since
  they all share the same shape: normal transaction + tip transfer, POSTed to a
  regional endpoint. Per-provider quirks that do differ (JSON-RPC vs a bare
  `{"transaction": ...}` body, auth header vs key-in-URL) are config fields.
- **Static nearest-first region ordering, not runtime region selection.** The
  frontend picks a region dynamically; the sniper runs on a fixed box in
  Hetzner FSN1, so the nearest region is known ahead of time and probing on the
  hot path would only add latency. Order the endpoint lists Frankfurt-first.

## Verified

### Jito
- **Frankfurt:** `https://frankfurt.mainnet.block-engine.jito.wtf`
  - single tx: `/api/v1/transactions` (add `?bundleOnly=true` for revert protection)
  - bundles: `/api/v1/bundles` (max 5 txs)
- Other regions: `amsterdam`, `dublin`, `london`, `ny`, `slc`, `singapore`, `tokyo`
- **Minimum tip:** 1000 lamports (competitive launches need far more)
- **Auth (if used) goes in the URL as `?uuid=<key>`**, matching our
  `jito-block-engine-adapter.ts`. Our frontend encodes the transaction as
  **bs58** with no options object; the sniper sends base64 with
  `{"encoding":"base64"}` — both are accepted by Jito.
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
*(verified against helius.dev/docs/sending-transactions/sender)*
- **Frankfurt:** `http://fra-sender.helius-rpc.com/fast` — regional endpoints
  are **HTTP** and intended for backends; the HTTPS
  `https://sender.helius-rpc.com/fast` auto-routes to the nearest region.
  Other regions: `ams`, `lon`, `ewr`, `slc`, `tyo`, `sg`.
- `/ping` on any regional host is a latency probe.
- **No API key** for Sender itself, no credits billed, available on the free
  plan. (An API key is still needed for the regular RPC you use for
  blockhashes.)
- **Rate limit: 50 TPS** by default — comfortably above a 30-buy burst, and far
  more usable than Jito's 1 req/s/region.
- **Minimum tip:** 0.001 SOL (Sender Max — enters the priority tip buffer and
  uses every pathway); 0.000005 SOL with `?swqos_only=true` (single fast path,
  no buffer). A priority fee is required *in addition to* the tip.
- **`?mev-protect=true`** routes around validators statistically linked to
  sandwich attacks. Works on both tiers and needs no request-body change —
  combine as `?swqos_only=true&mev-protect=true`. Worth enabling for buys.
- Request body is exactly what we send: JSON-RPC `sendTransaction` with base64
  and `{"encoding":"base64","skipPreflight":true,"maxRetries":0}`.
- ⚠️ **Helius Sender has its OWN tip accounts — not Jito's.** Tipping a Jito
  account here does not count as a Sender tip. All 10:
  ```
  4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE
  D2L6yPZ2FmmmTKPgzaMKdhu6EWZcTpLy1Vhx8uvZe7NZ
  9bnz4RShgq1hAnLnZbP8kbgBg1kEmcJBYQq3gQbmnSta
  5VY91ws6B2hMmBFRsXkoAAdsPHBJwRfBht4DXox3xkwn
  2nyhqdwKcJZR2vcqCyrYsaPVdAnFoJjiksCXJ7hfEYgD
  2q5pghRs6arqVjRvT5gfgWfWcHWmw1ZuCzphgd5KfWGJ
  3KCKozbAaF75qEU33jtzozcJ29yJuaLJTy2jFdzUY8bT
  4TQLFNWK8AovT1gFvda5jfw2oJeRMKEmw7aH6MGBJ3or
  4vieeGHPYPG2MmyPRcYjdiDmmhN3ww7hsFNap8pVN3Ey
  wyvPkWjVZz1M8fHQnMMCDTQDbkManefNNhweYk5WkcF
  ```

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
*(values below taken from our own `nozomi-adapter.ts` — production-verified)*
- Endpoint: `https://<region>.nozomi.temporal.xyz/api/sendTransaction2?c=<API_KEY>`
  — regions **Frankfurt**, Amsterdam, US East. `/ping` is a lightweight RTT
  probe with a 65 s keep-alive.
- ⚠️ **Not JSON-RPC.** The body is the **bare base64 transaction** with
  `Content-Type: text/plain`, and the response carries **no signature** —
  derive it from the transaction you signed. Use `format=raw` in
  `FAST_PROVIDERS`; sending JSON-RPC here fails silently.
- **API key is required** and goes in the URL as `?c=<uuid>`, not a header.
- **Minimum tip:** 1_000_000 lamports (0.001 SOL). Higher tip = higher queue
  priority.
- **17 tip accounts.** Their docs explicitly call for a *different* address per
  transaction to avoid write-CU exhaustion, so configure all of them and let
  the sniper spread the 30 wallets across the list:
  ```
  TEMPaMeCRFAS9EKF53Jd6KpHxgL47uWLcpFArU1Fanq
  noz3jAjPiHuBPqiSPkkugaJDkJscPuRhYnSpbi8UvC4
  noz3str9KXfpKknefHji8L1mPgimezaiUyCHYMDv1GE
  noz6uoYCDijhu1V7cutCpwxNiSovEwLdRHPwmgCGDNo
  noz9EPNcT7WH6Sou3sr3GGjHQYVkN3DNirpbvDkv9YJ
  nozc5yT15LazbLTFVZzoNZCwjh3yUtW86LoUyqsBu4L
  nozFrhfnNGoyqwVuwPAW4aaGqempx4PU6g6D9CJMv7Z
  nozievPk7HyK1Rqy1MPJwVQ7qQg2QoJGyP71oeDwbsu
  noznbgwYnBLDHu8wcQVCEw6kDrXkPdKkydGJGNXGvL7
  nozNVWs5N8mgzuD3qigrCG2UoKxZttxzZ85pvAQVrbP
  nozpEGbwx4BcGp6pvEdAh1JoC2CQGZdU6HbNP1v2p6P
  nozrhjhkCr3zXT3BiT4WCodYCUFeQvcdUkM7MqhKqge
  nozrwQtWhEdrA6W8dkbt9gnUaMs52PdAv5byipnadq3
  nozUacTVWub3cL4mJmGCYjKZTnE9RbdY5AP46iQgbPJ
  nozWCyTPppJjRuw2fpzDhhWbW355fzosWSzrrMYB1Qk
  nozWNju6dY353eMkMqURqwQEoM3SFgEKC6psLCSfUne
  nozxNBgWohjR75vdspfxR5H9ceC7XXH99xpxhVGt3Bb
  ```
- Note: the frontend's `NOZOMI_ENABLED` kill switch and `mode: "no-cors"` are
  browser-only concerns (CORS). Server-side from Rust neither applies, and we
  can read real status codes instead of opaque responses.

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
