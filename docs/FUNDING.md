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

## How the terminals actually detect this

Worth knowing before spending time or fees on hygiene, because the three
detectors key on different things and only one of them is about funding.

| Label | Provider's own definition | Trigger |
|---|---|---|
| **Bundled Tx Wallet** (GMGN) | "A single account combines multiple wallets' txs into one tx bundle, **processed in the same block**" | Same-block buys. Funding is irrelevant to it. |
| **Suspected Insider** (GMGN) | wallets sharing identical "**creation time, funding source and transfer time**" | The funding graph. This is the one hygiene here defeats. |
| **Sniper** (GMGN) | "Wallet who buys in **earlier blocks** after pool created" | Timing. Unavoidable by definition. |

TrenchRadar's bundle scanner is simpler still: it keys on **slot timing alone**
— buys "within the same 0.4 seconds or so" — and explicitly does *not* use Jito
bundle ids, funding-source analysis, or mixer tracing. It re-displays pump.fun's
own warning flags.

Three consequences:

1. **The bundled and sniper labels are unavoidable.** Thirty wallets buying in
   block +1 *is* the detection. No funding schedule changes it.
2. **The insider heuristic needs all three signals to coincide**, and one of
   them is funding *source*. A single source wallet funding thirty wallets
   shares a source however far apart the transfers are — so spacing alone does
   not break it. Multiple sources or a provider hop does.
3. **Uniform gaps are themselves a pattern.** Thirty transfers at exactly two
   hours apart is arguably more identifiable than thirty random ones; no
   organic wallet set behaves like a metronome. `DIST_MIN_DELAY_MS` /
   `DIST_MAX_DELAY_MS` are a range for this reason.

On Solana an account does not exist on chain until it is funded, so the funding
transaction *is* the creation time. Staggering funding staggers two of the three
signals for free. The one it cannot touch is funding source.

No provider documents a time threshold. The "wait N hours between wallets"
figure circulating in trenches is folklore, not a published rule.

### Spreading a funding run over days

`--max-wallets N` funds a random N of the wallets still short, then exits:

```
distribute --max-wallets 3 --execute
```

Schedule that rather than holding one process open for two days — a long
foreground run dies to a dropped session, a reboot or an OOM and resumes
nothing, whereas this is idempotent by construction. A crude randomised
schedule:

```cron
17 */5 * * *  cd /root/carbon && sleep $((RANDOM \% 3600)) && \
              FUNDING_KEYPAIR=/root/funding.json distribute --max-wallets 2 --execute
```

The truncation happens *after* the shuffle, so each run picks a random subset;
taking the first N of a sorted list would walk buyer-01..buyer-30 in order and
reintroduce the index correlation the shuffle exists to remove.

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

**Built — `private_fund`.** Register on the Houdini partner portal (a free tier
exists, no application needed) and set `HOUDINI_API_KEY` / `HOUDINI_API_SECRET`.

```
private_fund                      # quote only, creates nothing
private_fund --execute            # create orders, write deposits.txt
distribute --deposits deposits.txt --execute
private_fund --status-order <houdiniId>
```

Every order is created with `useXmr: true` and `anonymous: true` — that pair is
what buys the Monero route, and without them this is just a slower ordinary
swap.

Order creation and sending are separate commands on purpose: an unfunded order
simply expires, whereas a send is irreversible. The split means the deposit
addresses can be read and checked before a lamport moves.

Auth is `Authorization: <ApiKey>:<ApiSecret>`, plus three mandatory compliance
headers (`x-user-ip`, `x-user-agent`, `x-user-timezone`) — requests without
them are rejected with a 400. Override via `HOUDINI_USER_IP`,
`HOUDINI_USER_AGENT`, `HOUDINI_USER_TZ`.

Set `HOUDINI_REFUND_ADDRESS`. A swap that fails or lands outside its quoted
band has nowhere to return the principal without one; the tool warns loudly if
it is unset.

**Not exercised against the live API.** The schemas are transcribed from the
spec, so every response is validated rather than trusted and failures are loud
with the raw body attached. Most importantly `deposit_address_is_solana`
refuses any deposit address that is not a valid Solana pubkey: the deposit leg
is SOL, and a wrong-chain address would send funds somewhere unrecoverable.
Quote for one wallet before running thirty.

**Tradeoffs, honestly:**

- **Custody.** Funds sit with the provider mid-route. A direct transfer cannot
  fail in a way that loses the principal; this can.
- **Cost.** Thirty swaps pay thirty sets of fees plus spread — materially more
  than thirty transfers at ~0.000005 SOL each.
- **Time.** Privacy routing settles in minutes, not the sub-second of a
  transfer. Fund well before a launch, never during one.
- **Minimums.** Per-swap minimums may exceed a per-wallet top-up, which can
  force fewer, larger destinations and a second hop.

### Axiom

**Axiom does not do private distribution.** What it offers is operational
separation, not on-chain privacy: a public "alpha wallet" versus a private
"size wallet" so followers tracking you on Axiom's own leaderboard see one and
not the other, plus a burner-wallet workflow (fund, snipe, take profit, return,
retire).

Funding a burner from the size wallet is still a plain traceable transfer.
Axiom is explicitly non-custodial (Turnkey MPC, they never hold funds), which
is structurally incompatible with running a mixer. Their "privacy" is about not
being copy-traded, not about breaking the graph.

## Operational notes

- Fund **before** a launch, not during. A funding run and a snipe in the same
  minute correlates the two by timing regardless of amounts.
- Never fund the sniper wallets from an exchange withdrawal address directly:
  that ties the cluster to a KYC'd identity in one hop, and no downstream
  hygiene undoes it.
- Sweeping profits back to a single wallet re-links everything the funding side
  was careful about. The exit path deserves the same treatment as the entry.
