# Verification Status

What is actually proven about this code, what rests on assumption, and what has never been exercised.
Written because "36 tests pass" and "this works" are very different claims.

Last assessed: 2026-07-29, branch `sniper` @ `e0cad975`.

## Verified by test run

`cargo test -p pumpfun-sniper-example` — **36 passed, 0 failed**, 3m45s compile, zero errors.
Executed this session on Rust 1.88.0 / Solana 3.1.5.

| Group | Tests | What they pin |
|---|---|---|
| `pump::instructions` | 16 | Buy data layout vs decoder, account order vs `ArrangeAccounts`, account flags vs IDL, ATA/token-program pairing, WSOL helper layouts, `track_volume` single-byte encoding, mayhem `bonding_curve_v2` append, buyback recipient always from the authorised set |
| `pump::pdas` | 4 | Program IDs are mainnet, per-coin PDAs match mainnet, ATAs match mainnet, derivations deterministic |
| `pump::quote` | 4 | Fresh-curve quote, dev-buy price shift, slippage/allowance application, zero-input → zero-output |
| `sender::tpu` | 10 | Leader-schedule arithmetic, distinct-leader collapsing, short-schedule tolerance, slot coverage, socket resolution/dedup, QUIC port fallback, overflow degradation, undecodable-node handling |

Two of these are stronger than typical unit tests:

- `buy_reproduces_a_real_mainnet_instruction`
- `buy_v2_reproduces_a_real_mainnet_instruction`

They rebuild a buy and assert it matches a **real successful mainnet transaction**, account for
account. That is what makes the account-layout regression risk testable at all.

## Verified against mainnet (per `VERIFICATION.md`)

Done by pulling pump.fun's on-chain Anchor IDL plus real successful buys and running differential
`simulateTransaction`. Plain JSON-RPC over 443, which a sandboxed session can reach.

| Claim | Evidence |
|---|---|
| v1 buy needs **18** accounts, not 16 | 49 real successful buys + differential simulation. Found the bug; fixed in `4e910d8e`. |
| `remaining[0]` = `bonding_curve_v2`, read-only | Wrong address → `InvalidBondingCurveV2` (6074) |
| `remaining[1]` = a `Global.buyback_fee_recipients` member, **writable** | Read-only → `PrivilegeEscalation`; outside set → 6057 |
| `global_volume_accumulator` is read-only | IDL; a write lock serialises against every other pump buyer in the slot |
| v2 buy takes **27** accounts | All 27 derived addresses matched three real successful buys |
| v2's trailing `bonding_curve_v2` is optional | v2 promotes `buyback_fee_recipient` to a named account |
| v2 is ~21% of live buy traffic | 11 of 52 sampled buys |

## Assumed, not proven

| Assumption | Risk |
|---|---|
| **The two undocumented `remaining_accounts` stay as observed.** | They are in neither the IDL nor the decoder. A pump.fun program upgrade can change them with **no compile error and no test failure** — the hardcoded fixtures would keep passing while every live buy failed. Re-run the `VERIFICATION.md` procedure after any pump upgrade. This is the single highest-risk item in the codebase. |
| **v2 layout rests on IDL + 3 reproduced buys.** | `VERIFICATION.md` §3.3 differential simulation was run for v1 and **not repeated for v2**. Worth running on the box before scaling v2 size. |
| **Fast-provider tip accounts in `.env.example` are current.** | A wrong tip account is accepted and then silently never lands. Verify against each provider's docs. |
| **`Global` fields map to the curve as assumed.** | Read once at startup; if pump changes field semantics, quotes drift silently. |

## Never exercised

Nothing below has ever run:

- **No live snipe.** Not one transaction has been sent — not even at 0.001 SOL.
- **No simulate run against live geyser.** The `SNIPE` log path has never fired on a real launch.
- ~~**No gRPC connection from any environment.**~~ **Disproven 2026-07-29.** The handoff claimed a
  sandboxed session's HTTP(S) egress proxy cannot carry gRPC/HTTP-2 or raw TCP. Tested directly
  against `prism-frankfurt.constant-k.com:55577`: DNS resolves, TCP connects, TLS negotiates ALPN
  `h2`, and `grpcurl` streamed live entries. No proxy variables are set in this environment. Whatever
  was true of the session that wrote the handoff is not true here — **test reachability, don't assume
  it.**
- **No fast-provider send.** No provider endpoint has been contacted with a real transaction.
- **No Jito bundle submitted.**
- **No TPU/QUIC send.** The leader arithmetic is unit-tested; the socket path is not.
- **No end-to-end latency measurement.** Whether this lands in block 1 at all is unmeasured.

## Structural limits

| Limit | Consequence |
|---|---|
| **Geyser at `Processed` delivers a create after its block is built.** | Block 1 is the floor **on the Yellowstone feed**. Shredstream is the route to block 0 and is now wired in — see below. |
| **No exit path.** | It buys and holds. No position tracking, no auto-sell, no stop-loss. Every snipe is a one-way commitment. |
| **`sniped_mints` never shrinks.** | `MAX_POSITIONS` is a process-lifetime budget. Restart required to snipe again once hit. |
| **Unstaked QUIC is deprioritized by validators** | Exactly under the load a contested launch creates. Direct-TPU landing improves a lot with a staked identity or rented staked connections. |
| **No persistence.** | A restart loses all knowledge of held positions. With no exit path this is currently moot, but it blocks phase 2. |

## Shredstream feed — added 2026-07-29

`DATASOURCE=shredstream` was wired in this session (previously the unbuilt M3 milestone).

### Verified against the live endpoint

Endpoint `prism-frankfurt.constant-k.com:55577` (constant-k Prism, Frankfurt — the same metro as the
deployment target).

| Check | Result |
|---|---|
| DNS / TCP | resolves, connects |
| TLS | ALPN `h2`; certificate is a GlobalSign wildcard for `*.constant-k.com` |
| **Certificate verification** | **passes with standard roots — no `-insecure` and no bypass needed.** An earlier `openssl` "unable to get local issuer certificate" was this box's CA store, not the endpoint |
| `x-token` | **mandatory.** Without it the server returns a bare `HTTP 204 No Content`, which surfaces as `malformed header: missing HTTP content-type` — not as an auth error |
| gRPC reflection | disabled (204); protos must be supplied explicitly |
| Live data | 409 messages / 18 distinct slots / 17-slot span in ~15 s, ~23 entries per slot |
| Protos | `auth`/`shared`/`shredstream.proto` fetched from `jito-labs/mev-protos` into `misc/jito-protos/protos/`; `carbon-jito-protos` now builds (it could not before — the protos were absent from the repo) |

### What shredstream costs you

`SubscribeEntriesRequest` carries **no filters**, and shreds exist before execution. So on this feed:

| Property | Yellowstone | Shredstream |
|---|---|---|
| `meta.pre_balances` / `post_balances` | real | **empty** (`..Default::default()`) |
| `meta.status` | real | **always `Ok(())`** — a create that ultimately fails looks successful |
| `block_time` | real block time | **local receive time** |
| Server-side program filter | yes (`account_include`) | **none** — every network transaction is decoded locally |
| Confirmation | confirmed | **pre-confirmation — a create seen may never land** |
| Block 0 | unreachable | reachable |

Three of the nine guards depend on that metadata. They are now **skipped explicitly** under
`synthetic_meta` rather than evaluated against zeroes, and the dangerous combination is refused at
startup:

- **`MIN_CREATOR_BALANCE_SOL > 0` + shredstream → hard error.** With no balances, `pre` reads 0, so
  `0 < floor` is false and *every* launch is admitted while the operator believes a floor is in force.
- `MAX_CREATOR_BUY_SOL` — guard inactive (startup warning). The value is still used as the dev-buy
  fallback, which does not depend on metadata.
- `MAX_TX_AGE_MS` — freshness gate inactive (startup warning); every create measures ~0 ms old.

Dev-buy detection still works on shredstream — it reads instruction data, not metadata.

### End-to-end run — 2026-07-29

**The sniper ran against the live Frankfurt feed in dry run.** First time this product has executed
against real data.

```
sniper starting: datasource Shredstream, 1 watched creator(s), 1 buyer wallet(s) (DRY RUN)
shredstream: https://prism-frankfurt.constant-k.com:55577 (x-token set)
00:00:05   13722 processed (100%), 13722 successful, 0 failed, 0 in queue
00:00:10   29461 processed (100%), 29461 successful, 0 failed, 0 in queue
           jito_shredstream_grpc_entry_updates_received_total: 13037
           jito_shredstream_grpc_duplicate_entries_total: 0
```

Confirmed working: config validation and warnings, dry-run detection, wallet preflight (flagged an
underfunded wallet and continued, as intended in dry run), TLS + `x-token` connect, and the carbon
pipeline consuming shredstream updates.

**Throughput: ~2,950 transactions/sec, 0 failures, queue depth 0 — in a debug build.** That settles
the no-server-side-filter cost: decoding every transaction on the network keeps up with headroom.
Entry dedup reported 0 duplicates over the run.

### Still unverified on this feed

- **No snipe path exercised.** `WATCHED_CREATORS` was deliberately set to the system program as a
  control, so no create matched. The guards, quote, build, and dispatch stages did not run.
- **Still no transaction ever sent** — this was a dry run, and the wallet held 0 lamports.
- **Whether shredstream actually wins block 0 in practice** is unmeasured; no fill has been observed.
- Run was a debug build over ~75 s. Sustained behaviour and release-build headroom are unmeasured.

## Doc defects found in this repo

Discovered by reading the code against the shipped docs. **These are wrong in the branch's own
documentation:**

| Location | Claim | Reality |
|---|---|---|
| `examples/pumpfun-sniper/README.md` §Not yet implemented | "`create_v2` coins: logged and skipped; buys use the v1 instruction only" | **False.** v2 is implemented; `SNIPE_V2` defaults to **on**. Added in `64c05118`. |
| `examples/pumpfun-sniper/README.md` step 4 | "sent per `SEND_MODE`: `simulate`, `rpc`, or `jito`" | **False and dangerous.** `SEND_MODE` is only `simulate` vs live — *any* non-`simulate` value goes live. Routes come from `SEND_PATHS`. Following this README puts you live believing you picked a route. |
| `SNIPER_HANDOFF.md` §8 Known residual risks | "`create_v2` / mayhem coins are unsupported" | **False**, and contradicts §3 of the same file. Stale entry. |
| `SNIPER_HANDOFF.md` §3 | "11 unit tests cover the leader arithmetic" | 10 `sender::tpu` tests exist. Trivial, noted for accuracy. |
| `SNIPER_HANDOFF.md` §1 | old branches "share **no merge base** with current `main`" | They do share one (`4e7c612b`), just very old. The practical point — that the old buy transactions no longer work — stands. |

## Go-live checklist

From `SNIPER_HANDOFF.md` §6, with current status. **Only step 1 is done.**

| # | Step | Status |
|---|---|---|
| 1 | `cargo test -p pumpfun-sniper-example` passes | ✅ verified 2026-07-29, 36/36 |
| 2 | Run the `VERIFICATION.md` procedure **on the box** — diff account lists and signer/writable flags against recent real buys | ❌ not done |
| 3 | `SEND_MODE=simulate` against live geyser; confirm `SNIPE` fires and simulation returns OK on a real launch | ⚠️ **half done** — ran in dry run against live shredstream 2026-07-29, 29,461 updates / 0 failures. But `WATCHED_CREATORS` was a control value, so **no `SNIPE` fired and no simulation ran.** Needs a real watched creator to complete. |
| 4 | One live snipe at `BUY_AMOUNT_SOL=0.001`, `SEND_PATHS=rpc`; verify the fill on-chain | ❌ not done |
| 5 | Fill in real tip accounts for each `FAST_PROVIDERS` entry from that provider's docs | ❌ not done |
| 6 | Only then scale size, wallet count, and enable `fast`/`jito`/`tpu` | ❌ not done |

Steps 2–4 require the deployment box — they cannot be done from a sandboxed session, because
Yellowstone is gRPC.

## Honest summary

The code compiles, its tests pass, and its riskiest component — the buy account layout — has been
checked against real mainnet transactions for both launch flows and pinned by tests. That is
meaningfully better than untested code.

It has also never sent a transaction, cannot reach block 0, and cannot sell what it buys. Treat
"tests pass" as evidence the buys are *constructed* correctly, not evidence the product works.
