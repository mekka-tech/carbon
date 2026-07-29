# pump.fun buy-instruction verification

Audit of `src/pump/instructions.rs` + `src/pump/pdas.rs` against pump.fun as
actually deployed on Solana mainnet.

**Date of verification: 2026-07-29.** Everything below was checked against
mainnet at that time. Facts marked *snapshot* can be changed by the pump
authority at any moment and must be re-checked before a live run.

---

## 0. TL;DR — what was wrong

| # | Defect | Impact | Status |
|---|---|---|---|
| 1 | Only 16 accounts were passed. The deployed program requires 18: `bonding_curve_v2` then a buyback fee recipient, read out of `remaining_accounts`. | **Every buy failed on-chain** with `BuybackFeeRecipientMissing` (6062). Reproduced by simulation. | Fixed |
| 2 | `global_volume_accumulator` was marked writable. | Not fatal, but takes a write lock on an account shared by *every* pump buyer in the slot, serialising us against the whole market. IDL declares it read-only. | Fixed (read-only) |
| 3 | Token program was hardcoded to SPL Token with no way to override, and the ATA seed used the same hardcode. | Correct for the coins this sniper buys (`create` = SPL Token), but silently wrong for `create_v2` (Token-2022), which is ~80% of current buy volume. Would fail with `ConstraintAssociatedTokenTokenProgram` (2023). | Made explicit + overridable |
| 4 | `StaticAccounts` had no buyback-recipient source. | Needed for fix #1. | Added, with `from_global()` to read live values |

Account **order**, **discriminator**, and **all PDA seeds** were already correct.

---

## 1. What was verified, and how

### 1.1 Source of truth: the on-chain Anchor IDL

The repo has no pump IDL, so the authoritative one was pulled straight off
mainnet from pump's Anchor IDL account:

```
base       = find_program_address([], 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P)
idl_account= create_with_seed(base, "anchor:idl", pump_program_id)
           = AYgC53tU5BbP2NAnv5nConJxAdpQZctvmZK88pu69xRs
```

Layout: `8 byte discriminator | 32 byte authority | u32 len | zlib(json)`.
See §3.1 for the copy-pasteable command.

The IDL's `buy_exact_sol_in` matches `decoders/pumpfun-decoder/src/instructions/buy_exact_sol_in.rs`
**exactly** — same 16 accounts in the same order, same discriminator
`[56, 252, 116, 8, 158, 223, 205, 95]`, same args
`(spendable_sol_in: u64, min_tokens_out: u64, track_volume: OptionBool)`.
The Codama decoder is therefore trustworthy for order and args. It is **not**
complete for the deployed program — see §1.3.

### 1.2 Discriminator

`sha256("global:buy_exact_sol_in")[..8] == [56, 252, 116, 8, 158, 223, 205, 95]`,
which matches the constant in `instructions.rs`, the decoder's `decode()` check,
the on-chain IDL, and the first 8 bytes of every real buy observed. Locked in by
`buy_data_layout_matches_the_decoder`.

### 1.3 Account list vs. reality (the big one)

9 consecutive mainnet blocks (slots **435898359 – 435898367**) were fetched in
full and every `buy` / `buy_exact_sol_in` instruction in a successful
transaction extracted: **49 instructions, and all 49 carried 18 accounts, not
16.** The two extra accounts are not in the IDL; the program reads them from
`remaining_accounts`.

They were identified by differential simulation against mainnet (replaying a
real, successful buy with one thing changed at a time):

| Variant | Result |
|---|---|
| 18 accounts, unmodified | `err: null` (succeeds) |
| **16 accounts (the old builder)** | **`BuybackFeeRecipientMissing` (6062)** |
| 17 accounts (drop the last) | `BuybackFeeRecipientMissing` (6062) |
| 17 accounts, buyback moved into slot 16 | `BuybackFeeRecipientMissing` (6062) |
| slot 16 → system program | `InvalidBondingCurveV2` (6074) |
| slot 16 → the mint | `InvalidBondingCurveV2` (6074) |
| slot 17 → a non-buyback fee recipient | `BuybackFeeRecipientNotAuthorized` (6057) |
| slot 17 → each of the 8 authorised recipients | all succeed |
| slot 17 made read-only | `PrivilegeEscalation` |
| slot 16 made writable | succeeds (writable is tolerated, read-only is what real traffic uses) |

So the required layout is:

* `remaining[0]` = **`bonding_curve_v2`** = `find_program_address(["bonding-curve-v2", mint], pump)` — read-only.
  Confirmed for 43/43 ordinary buys in the sample. The other 6 were "mayhem
  mode" CPI buys (`user == Global.whitelist_pda`), which derive the same PDA on
  the mayhem program `MAyhSmzXzV1pTf7LsNkrNwkWKTo4ougAJ1PPg47MD4e`. The sniper
  never takes that path.
* `remaining[1]` = **buyback fee recipient**, writable, must be one of the 8
  entries in `Global.buyback_fee_recipients`. All 49 observed values were from
  that set.

The first 16 accounts, their order, and which pubkey we pass for each were
already correct and are unchanged.

### 1.4 Signer / writable flags — now measured, not inferred

Flags come from the IDL (`writable` / `signer` per account) and were each
confirmed by simulation.

| # | Account | signer | writable | Evidence |
|---|---|---|---|---|
| 0 | global | no | no | IDL; 49/49 observed read-only |
| 1 | fee_recipient | no | **yes** | IDL; 49/49 |
| 2 | mint | no | no | IDL; 49/49 |
| 3 | bonding_curve | no | **yes** | IDL; 49/49 |
| 4 | associated_bonding_curve | no | **yes** | IDL; 49/49 |
| 5 | associated_user | no | **yes** | IDL; 49/49 |
| 6 | user | **yes** | **yes** | IDL; 43/49 signer (the other 6 are CPI buys where `user` is a PDA — not our case) |
| 7 | system_program | no | no | IDL; 49/49 |
| 8 | token_program | no | no | IDL; 49/49 |
| 9 | creator_vault | no | **yes** | IDL; 49/49 |
| 10 | event_authority | no | no | IDL; 49/49 |
| 11 | program | no | no | IDL; 49/49 |
| 12 | global_volume_accumulator | no | **no** | IDL says read-only. 35/49 observed read-only; simulation succeeds both ways. **We now send read-only.** |
| 13 | user_volume_accumulator | no | **yes** | IDL; 49/49 |
| 14 | fee_config | no | no | IDL; 49/49 |
| 15 | fee_program | no | no | IDL; 49/49 |
| 16 | bonding_curve_v2 | no | no | 49/49; writable also accepted |
| 17 | buyback_fee_recipient | no | **yes** | 49/49; read-only → `PrivilegeEscalation` |

Note on reading flags out of a transaction: `isSigner` / `isWritable` in
`getTransaction` are *message*-level and are the union over every instruction in
the transaction. A key showing writable there does not prove the buy needs it
writable (this is why index 12 shows writable in some transactions). A key
showing **read-only** in a successful transaction *is* proof that read-only is
sufficient.

Confidence in the flags: **high**. Every one is corroborated by both the IDL and
executed mainnet simulation of this exact layout.

### 1.5 Argument encoding

`OptionBool` is declared in the IDL as a struct with a single `bool` field, so
borsh encodes it as **one byte**. The decoder's hand-written `BorshDeserialize`
reads one byte and tolerates EOF (older buys omit the byte entirely). Our
derived `BorshSerialize` emits exactly one byte, so we are byte-compatible in
both directions.

Total data length: `8 + 8 + 8 + 1 = 25` bytes. Simulation confirms the program
accepts 24 bytes (no trailing byte), 25 with `0`, and 25 with `1`. Real traffic
contains all of these.

### 1.6 PDA derivations — all confirmed twice

Each seed string below is taken verbatim from the on-chain IDL's `pda.seeds`,
and each derived address was checked to equal the corresponding account in a
real mainnet buy.

| PDA | Seeds | Program | Confirmed value |
|---|---|---|---|
| `global` | `["global"]` | pump | `4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf` |
| `event_authority` | `["__event_authority"]` | pump | `Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1` |
| `global_volume_accumulator` | `["global_volume_accumulator"]` | pump | `Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y` |
| `user_volume_accumulator` | `["user_volume_accumulator", user]` | pump | `574dmPVqZGSsXrebbxRvRxqN3HM2oBk5CAgWdb5Lxixg` |
| `creator_vault` | `["creator-vault", bonding_curve.creator]` | pump | `7gNNmYipDqGPE5uTmjdy8i6ommvVirvGnrhCsKK4fHdN` |
| `bonding_curve` | `["bonding-curve", mint]` | pump | `4vscQFvtKuQ4gVSyMFQgR8NP1erfUxTgwtfRtbTW5cfr` |
| `bonding_curve_v2` | `["bonding-curve-v2", mint]` | pump | `HnWSmuLBVrahUf5rhV26ra7ki6iTZxVhJNSP5cQJ2q5G` |
| `fee_config` | `["fee_config", <pump program id bytes>]` | **fee program** `pfeeUxB6…` | `8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt` |
| `associated_bonding_curve` | `[bonding_curve, token_program, mint]` | ATA program | `Amj4ZUv1zwxMnVBSe5wrGC4zwzneNexKS77F7KC64JQB` |
| `associated_user` (buyer ATA) | `[owner, token_program, mint]` | ATA program | `DP8pRnKUz6vgMFMAofm5tFPNuhVoQNraLBqeKu6cniuF` |

`fee_config` was the one flagged as uncertain in the offline pass: it is now
**confirmed** — the second seed is literally the 32 raw bytes of the pump
program id, and the derivation program is the fee program, exactly as
`pdas::fee_config()` does it.

### 1.7 `create_ata_idempotent`

* Data byte `1` = `CreateIdempotent`. Simulation: running it twice in one
  transaction over an existing ATA succeeds. Data byte `0` (`Create`) on an
  existing ATA fails with `IllegalOwner` — so the idempotent variant is
  required, and we use it.
* Account order `payer(s,w), ata(w), owner(r), mint(r), system(r), token(r)` is
  correct: swapping `owner` and `mint` fails with
  `InvalidSeeds` / "Associated address does not match seed derivation".
* The ATA it creates is asserted equal to the ATA the buy instruction uses
  (`ata_created_matches_the_ata_the_buy_uses`).

### 1.8 Token program

The IDL hardcodes the token program per instruction:

* `create` → `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA` (SPL Token)
* `create_v2` → `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb` (Token-2022)

`processor.rs` only emits snipe signals for `Create`, so hardcoding SPL Token is
correct **today**. In the sampled traffic, 40 of 49 buys were against Token-2022
mints, so this will bite the moment `create_v2` support is added. The token
program is now a field on `CoinAccounts` (defaulting to SPL Token via
`CoinAccounts::new`, overridable via `CoinAccounts::new_with_token_program`) and
it drives both the account at index 8 and the ATA seeds.

### 1.9 End-to-end proof

The exact 18 `AccountMeta`s and the exact 25 data bytes the fixed builder emits
were reassembled and simulated on mainnet:

```
FINAL: exact output of our fixed builder    err=None   unitsConsumed=68722
REGRESSION: old 16-account builder          err=BuybackFeeRecipientMissing (6062)
```

### 1.10 Transactions used

| Signature | Slot | Role |
|---|---|---|
| `4iTDfoKjZF22dF9VdbskGY8irmZwNGj2ECzHSKjeaXPHqsGGabiYMRwLP1WpUwxf6oCCCAK6C9jTnPnX5nPHenuZ` | 435898365 | Primary fixture. SPL-Token coin, top-level `buy_exact_sol_in`, 18 accounts. Encoded verbatim in `instructions.rs` tests and used as the simulation base for every differential test above. |
| `48VKGEAnRxMX5HkfJL3YUyhBQgeNS2nsp67P5euZ1DfZFddixyN3cjp9ZeY7hyPHSCxNPCQRJfe3fyVuspg5Rc9s` | 435898167 | First independent sample; Token-2022 + mayhem-mode coin. Established that 18 accounts is not specific to one client. |
| `4a2gSYkJJibExAhrtR5L3t97yTxDSwoCLwgSZGJB47TMTawAatFtVPkKxutjrWudEugvy87kdoNEprjfHT8rCHUA` | 435898359–67 | Token-2022 `buy_exact_sol_in`, 24-byte data (no `track_volume` byte). |
| `Hxo3QnyeotxmUp4imfJV9SjPCNPeFqf4fAn6tsenMjqHGUguKK2s7vNFoHam7z5pzwbmEwK88FcZMbdCB9kcgnu` | 435898359–67 | Mayhem-mode CPI buy; the case where `bonding_curve_v2` is derived on the mayhem program. |

Plus the full contents of slots 435898359–435898367 (49 buy instructions in
total) for the statistics quoted above.

---

## 2. Regression tests

`cargo test -p pumpfun-sniper-example` — 29 tests, all passing.

In `src/pump/instructions.rs`:

* `buy_reproduces_a_real_mainnet_instruction` — all 18 accounts, in order, byte-equal to the fixture transaction.
* `buy_account_flags_match_the_idl` — signer/writable for all 18; exactly one signer and it is the buyer.
* `buy_data_layout_matches_the_decoder` — length, discriminator prefix, both u64s, the `OptionBool` byte, and a round-trip through the decoder's own `BuyExactSolIn::decode`.
* `track_volume_serialises_as_a_single_byte`.
* `buy_account_order_matches_the_decoders_arrange_accounts` — feeds our instruction into the decoder's `ArrangeAccounts` and asserts every named field plus `remaining.len() == 2` in order.
* `buyback_recipient_is_always_from_the_authorised_set` — 64 synthetic buyers, all land on an authorised writable recipient and the choice spreads over all 8.
* `token_program_drives_the_buyer_ata`.
* `create_ata_idempotent_layout`, `create_ata_idempotent_honours_the_token_program`, `ata_created_matches_the_ata_the_buy_uses`.

In `src/pump/pdas.rs`: `program_level_pdas_match_mainnet`,
`per_coin_pdas_match_mainnet`, `associated_token_addresses_match_mainnet`,
`derivations_are_deterministic`, `program_ids_are_the_mainnet_ones`.

---

## 3. Re-running this on the deployment box

Anything below can be run wherever RPC is reachable. Set `RPC` first:

```bash
RPC=https://api.mainnet-beta.solana.com   # or your private endpoint
PUMP=6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
```

### 3.1 Re-fetch the on-chain IDL

```bash
python3 - <<'EOF'
import hashlib, json, urllib.request, base64, zlib
RPC="https://api.mainnet-beta.solana.com"
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58d(s):
    n=0
    for c in s: n=n*58+A.index(c)
    return bytes(len(s)-len(s.lstrip('1')))+(n.to_bytes((n.bit_length()+7)//8,'big') if n else b'')
def b58e(b):
    n=int.from_bytes(b,'big'); s=''
    while n: n,r=divmod(n,58); s=A[r]+s
    return '1'*(len(b)-len(b.lstrip(b'\0')))+s
def rpc(m,p):
    req=urllib.request.Request(RPC,json.dumps({"jsonrpc":"2.0","id":1,"method":m,"params":p}).encode(),
                               {"Content-Type":"application/json"})
    return json.load(urllib.request.urlopen(req))
PUMP=b58d('6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P')
# Anchor IDL address = create_with_seed(find_program_address([], pump), "anchor:idl", pump)
p=2**255-19; d=(-121665*pow(121666,p-2,p))%p
def on_curve(b):
    y=int.from_bytes(b,'little')&((1<<255)-1); sign=b[31]>>7
    if y>=p: return False
    y2=y*y%p; u=(y2-1)%p; v=(d*y2+1)%p
    x=u*pow(v,3,p)*pow(u*pow(v,7,p),(p-5)//8,p)%p
    if (v*x*x-u)%p==0: pass
    elif (v*x*x+u)%p==0: x=x*pow(2,(p-1)//4,p)%p
    else: return False
    return not (x==0 and sign)
def fpa(seeds,prog):
    for bump in range(255,-1,-1):
        h=hashlib.sha256()
        for s in seeds: h.update(s)
        h.update(bytes([bump])); h.update(prog); h.update(b'ProgramDerivedAddress')
        c=h.digest()
        if not on_curve(c): return b58e(c),bump
base,_=fpa([],PUMP)
idl=b58e(hashlib.sha256(b58d(base)+b"anchor:idl"+PUMP).digest())
print("IDL account:",idl)
raw=base64.b64decode(rpc("getAccountInfo",[idl,{"encoding":"base64"}])["result"]["value"]["data"][0])
ln=int.from_bytes(raw[40:44],'little')
open("pump_idl.json","wb").write(zlib.decompress(raw[44:44+ln]))
j=json.load(open("pump_idl.json"))
for ix in j["instructions"]:
    if ix["name"]=="buy_exact_sol_in":
        for i,a in enumerate(ix["accounts"]):
            print(i,a["name"],"W" if a.get("writable") else "r","S" if a.get("signer") else "")
        print("args:",[(x["name"],x["type"]) for x in ix["args"]])
EOF
```

Compare that printout against `FIXTURE_ACCOUNTS` / `FIXTURE_FLAGS` in
`src/pump/instructions.rs`. **If the IDL grows or reorders accounts, the builder
must be updated** — but note the IDL is *not* sufficient on its own (§1.3), so
also do §3.2.

### 3.2 Diff real buys against our layout

Pulls recent successful pump buys and prints their account lists with flags:

```bash
python3 - <<'EOF'
import json, urllib.request, collections
RPC="https://api.mainnet-beta.solana.com"
PUMP="6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58d(s):
    n=0
    for c in s: n=n*58+A.index(c)
    return bytes(len(s)-len(s.lstrip('1')))+(n.to_bytes((n.bit_length()+7)//8,'big') if n else b'')
def rpc(m,p):
    req=urllib.request.Request(RPC,json.dumps({"jsonrpc":"2.0","id":1,"method":m,"params":p}).encode(),
                               {"Content-Type":"application/json"})
    return json.load(urllib.request.urlopen(req))
DISC={(56,252,116,8,158,223,205,95):"buy_exact_sol_in",(102,6,61,18,1,218,235,234):"buy"}
# One full block yields far more buys than paging getSignaturesForAddress, and
# costs a single RPC call. Only a fraction of blocks contain a *top-level* pump
# buy, so walk back a few slots until 3 are found.
tip=rpc("getSlot",[{"commitment":"finalized"}])["result"]-4
shown=0
for slot in range(tip, tip-25, -1):
    r=rpc("getBlock",[slot,{"encoding":"jsonParsed","maxSupportedTransactionVersion":0,
                            "transactionDetails":"full","rewards":False}])
    if "error" in r: continue
    for tx in r["result"]["transactions"]:
        if tx["meta"]["err"]: continue
        msg=tx["transaction"]["message"]
        flags={k["pubkey"]:(k["signer"],k["writable"]) for k in msg["accountKeys"]}
        la=tx["meta"].get("loadedAddresses") or {"writable":[],"readonly":[]}
        for k in la["writable"]: flags[k]=(False,True)
        for k in la["readonly"]: flags[k]=(False,False)
        for ix in msg["instructions"]:
            if ix.get("programId")!=PUMP: continue
            data=b58d(ix["data"]); name=DISC.get(tuple(data[:8]))
            if not name: continue
            print("==",name,tx["transaction"]["signatures"][0],"slot",slot,
                  "accounts:",len(ix["accounts"]),"data_len:",len(data))
            for i,a in enumerate(ix["accounts"]):
                s,w=flags[a]
                print(f"  {i:2d} {a:45s} {'S' if s else ' '}{'W' if w else 'r'}")
            shown+=1
            if shown>=3: raise SystemExit
EOF
```

Diff the printed lists against `FIXTURE_ACCOUNTS` / `FIXTURE_FLAGS`. Expect:
identical count (18), identical ordering by role, and identical flags except
possibly index 12 (see §1.4 on message-level flags).

### 3.3 Differential simulation (the strongest check)

Take any successful buy above, rebuild it, mutate one thing, and simulate. This
is how every claim in §1.3/§1.4 was established, and it needs no funded wallet —
`sigVerify: false` + `replaceRecentBlockhash: true`, with the buyer as fee payer:

```bash
curl -s -X POST "$RPC" -H 'Content-Type: application/json' -d '{
  "jsonrpc":"2.0","id":1,"method":"simulateTransaction",
  "params":["<base64 tx>",{"sigVerify":false,"replaceRecentBlockhash":true,
                           "encoding":"base64","commitment":"processed"}]}'
```

Prepend a `CreateIdempotent` ATA instruction (the buyer's ATA may have been
closed since), then check `result.value.err == null` and read
`result.value.logs` for the Anchor error name on failure.

### 3.4 Confirm the buyback recipients are still the ones we hardcode

**Do this before every live run** — this list is the most likely thing to drift:

```bash
curl -s -X POST "$RPC" -H 'Content-Type: application/json' -d \
  '{"jsonrpc":"2.0","id":1,"method":"getAccountInfo","params":["4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf",{"encoding":"base64"}]}' \
| python3 -c "
import sys,json,base64
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58e(b):
    n=int.from_bytes(b,'big'); s=''
    while n: n,r=divmod(n,58); s=A[r]+s
    return '1'*(len(b)-len(b.lstrip(b'\0')))+s
d=base64.b64decode(json.load(sys.stdin)['result']['value']['data'][0])
o=8+1+32+32+8*5+32+1+8+8+32*7+32+32+1+32+32+1+32*7+1   # -> buyback_fee_recipients
print('buyback_fee_recipients:')
for i in range(8): print(' ', b58e(d[o+32*i:o+32*(i+1)]))
"
```

Compare against `pdas::BUYBACK_FEE_RECIPIENTS`. If it differs, either update the
constant **or** (better) change `main.rs` to build statics with
`StaticAccounts::from_global(&global)` instead of `StaticAccounts::new(...)` —
`main.rs` already decodes `Global`, so this is a one-line change that removes
the snapshot entirely. `main.rs` was out of scope for this audit.

---

## 4. Assumptions that remain unverified (silent-failure candidates)

Ordered by risk.

1. **The buyback recipient list is a snapshot.** `pdas::BUYBACK_FEE_RECIPIENTS`
   was read from mainnet on 2026-07-29. `update_buyback_config` can rotate it at
   any time; if it does, **every buy fails** with
   `BuybackFeeRecipientNotAuthorized` (6057). Mitigation: §3.4, and wiring
   `StaticAccounts::from_global`. Currently `main.rs` calls
   `StaticAccounts::new(global.fee_recipient)`, which uses the snapshot.

2. **`bonding_curve_v2` and the buyback recipient are undocumented
   `remaining_accounts`.** They are not in the IDL, so a program upgrade can add,
   reorder or drop them with no IDL signal and no compile-time error here. Only
   §3.2/§3.3 will catch it. The IDL predates the deployed binary in this respect
   (the error strings come from `programs/pump/src/sell.rs`, i.e. shared
   buy/sell code the IDL does not describe).

3. **`min_tokens_out == 0` is rejected on-chain** with `BuyZeroAmount` (6020) —
   confirmed by simulation. `quote::min_tokens_out` can legitimately return 0
   (100% slippage, or a dust buy). Quoting behaviour was left unchanged because
   it is outside this audit's scope, but a buy quoted at 0 will always fail. Worth
   clamping to 1 in `quote.rs` or rejecting the buy in `dispatch.rs`.

4. **`create_ata_idempotent` is still called with the default SPL Token
   program** from `dispatch.rs` (`create_ata_idempotent(&buyer, &buyer, &mint)`).
   That matches `CoinAccounts::new`'s default and is correct for `create` coins.
   If `create_v2` support is ever added, `dispatch.rs` must switch to
   `create_ata_idempotent_with_program(..., &coin.token_program)` or the buy and
   the ATA will disagree. `dispatch.rs` was out of scope.

5. **Fee-recipient selection.** We always pass `Global.fee_recipient`. That
   pubkey is accepted (it appeared in 12/49 real buys), but real clients rotate
   across `Global.fee_recipients[7]` too. Not a correctness problem; it does mean
   all our wallets write-lock the same fee recipient account.

6. **Curve constants for quoting.** `min_tokens_out` is computed from
   `Global.initial_virtual_*` and `fee_basis_points` / `creator_fee_basis_points`
   read at startup, plus an estimate of the creator's dev buy. If the real curve
   has moved more than the estimate, the buy fails with
   `BuySlippageBelowMinTokensOut` (6042) — verified as the actual error for a
   too-high `min_tokens_out`. This is a live-tuning matter, not a layout bug.

7. **`create_v2` / mayhem-mode coins are not buyable by this sniper.**
   `processor.rs` logs and skips them. For those coins `bonding_curve_v2` is
   derived on the mayhem program and the buy instruction differs
   (`buy_v2` takes 27 accounts). Nothing here supports that path.

8. **Everything is pinned to one point in time.** The pump program is upgradeable
   (it has an IDL authority and an `admin_set_idl_authority` instruction). The
   fixture test will keep passing after a program upgrade — it only checks that
   *we* still build what we intended, not that the program still wants it. §3.2
   and §3.3 are the only defence against an upgrade, and should be re-run before
   any live session.
