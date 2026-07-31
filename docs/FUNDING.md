# Funding the sniper wallets

## What is actually being hidden

Thirty wallets buying one mint inside two blocks is a stronger fingerprint than
any funding graph. Anyone watching pump.fun launches clusters the set from the
buys alone — same mint, same slot, same size profile — regardless of how the
wallets were funded. **Funding privacy does not hide the snipe.**

What it does buy is unlinking the *cluster* from the source wallet: the capital
base, the history across launches, and the ability of anyone who identifies one
snipe to then enumerate every other one you have run. That is a real goal. It
is just narrower than "distribute privately", and worth being precise about
before paying swap fees for it.

## Direct distribution — `distribute`

```
distribute --list                   # wallets, balances, what each needs
distribute                          # print the plan, send nothing
distribute --execute                # send
distribute --target 0.2 --execute   # top every wallet up to ~0.2 SOL
```

Reads `RPC_URLS`, `BUYER_KEYPAIR_DIR`, `FUNDING_KEYPAIR` (a keypair file path;
or `FUNDING_KEYPAIR_B58` for a base58 secret).

Three properties, each of which exists to defeat a specific clustering method:

1. **One transfer per transaction.** A single transaction carrying thirty
   `SystemProgram::Transfer` instructions into thirty fresh addresses labels
   the entire set as one entity, in one instruction list, permanently. This is
   the single most important rule here and the easiest one to get wrong by
   optimising for fees.
2. **Amounts jittered ±12% from OS entropy.** Thirty transfers of exactly
   0.15 SOL cluster as well as one batched transaction does. The jitter is a
   uniform draw across the whole band — an earlier implementation returned the
   exact target for half of all draws, which is the very fingerprint it was
   meant to remove, and a bounds-only test passed on it. `jitter_covers_both_sides_of_the_target`
   pins the distribution now, not just the range.
3. **Shuffled order, random 0.8–4s gaps.** So the on-chain sequence carries no
   information about which wallet holds which index.

Tunable: `DIST_TARGET_SOL`, `DIST_JITTER_PCT`, `DIST_MIN_DELAY_MS`,
`DIST_MAX_DELAY_MS`.

Re-running is safe and cheap. Amounts are computed from live balances, so a
second run tops up only what is still short — which is also the recovery path
when some sends fail part way through.

**What this does not do:** every lamport still traces to the source in one hop.
Direct distribution defeats casual clustering and naive bots. It does not
defeat anyone who looks.

## Breaking the link — provider deposits

To actually sever the graph you need a route through something with a real
anonymity set. `distribute` takes the destination list from a file, so it
becomes the sender for any provider flow without changes:

```
distribute --deposits deposits.txt --execute
```

`deposits.txt` is `address,amount_sol` per line, `#` for comments. Amounts are
used **verbatim** — a provider quote is exact, and jittering it would land
outside the quoted band and fail the swap. The one-transfer-per-transaction,
shuffle and delay rules still apply.

### Houdini Swap

Verified against their OpenAPI spec at
`https://api-partner.houdiniswap.com/v2/openapi.json`. Partner API key
required, sent in the `Authorization` header.

| Step | Endpoint |
|---|---|
| Price a swap | `GET /quotes` — `amount`, `from`, `to` token ids; optional `slippage`, `fixed`, `refundAddress` |
| Create one order | `POST /exchanges` — `{ addressTo, quoteId }` → order with a deposit address |
| Create up to 50 at once | `POST /exchanges/multi` — `orders[]`, returns `multiId` + per-order results |
| Track a batch | `GET /exchanges/multi/{multiId}` → `bundleStatus` |
| Track one order | `GET /orders/{houdiniId}` |

`POST /exchanges/multi` accepts 1–50 orders, so all thirty wallets fit in one
batch. The flow is: create the batch → collect one deposit address per order →
write them to `deposits.txt` → `distribute --deposits`. Houdini routes and
delivers to each `addressTo`.

**Not yet built.** The endpoints above are transcribed from their spec and are
not exercised by any code here, because that needs a partner key to test
against. Untested request-building on a path that moves real money is worse
than no code, so it waits for a key.

**Tradeoffs, honestly:**

- **Custody.** Funds sit with the provider mid-route. A direct transfer cannot
  fail in a way that loses the principal; this can.
- **Cost.** Thirty swaps pay thirty sets of fees plus spread — materially more
  than thirty transfers at ~0.000005 SOL each.
- **Time.** Privacy routing settles in minutes, not the sub-second of a
  transfer. Fund well before a launch, never during one.
- **Minimums.** Per-swap minimums may exceed a per-wallet top-up, which can
  force fewer, larger destinations and a second hop.

### Axiom's provider

Mentioned as an option but **not verified** — no endpoint, auth scheme or
minimums confirmed, so nothing is written here rather than guessed. Point at
its docs and it slots into the same `--deposits` seam.

## Operational notes

- Fund **before** a launch, not during. A funding run and a snipe in the same
  minute correlates the two by timing regardless of amounts.
- Never fund the sniper wallets from an exchange withdrawal address directly:
  that ties the cluster to a KYC'd identity in one hop, and no downstream
  hygiene undoes it.
- Sweeping profits back to a single wallet re-links everything the funding side
  was careful about. The exit path deserves the same treatment as the entry.
