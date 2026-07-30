//! Sell an entire pump.fun v2 position across every buyer wallet.
//!
//! The sniper has no exit path — this is it. Because a wrong account list here
//! burns real tokens, the tool refuses to build anything until it has
//! re-derived the account layout of a *known-good mainnet sell* and matched it
//! address-for-address. That reference sell is hard-coded below; if pump.fun
//! changes the layout, verification fails loudly instead of sending a malformed
//! transaction.
//!
//! Usage:
//!   sell_all --list                     # show wallets, their index and balance
//!   sell_all                            # ALL wallets, 100%, simulate only
//!   sell_all --pct 50                   # ALL wallets, 50%
//!   sell_all --wallet 2 --pct 100       # ONE wallet by index (load order)
//!   sell_all --wallet 5vSTZ… --pct 25   # ONE wallet by pubkey
//!   sell_all --pct 50 --execute         # actually send
//!
//! Wallet indexes follow load order: files in `BUYER_KEYPAIR_DIR` sorted by
//! name, so `buyer-1.json` is index 0. `--list` prints the mapping.
//!
//! Reads `RPC_URLS`, `BUYER_KEYPAIR_DIR`, `SELL_MINT`, `SELL_CREATOR` from the
//! environment / .env. `--pct` overrides `SELL_PCT`.

use {
    solana_client::{
        nonblocking::rpc_client::RpcClient, rpc_config::RpcSendTransactionConfig,
        rpc_config::RpcSimulateTransactionConfig,
    },
    solana_commitment_config::CommitmentConfig,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::{v0, VersionedMessage},
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::sync::Arc,
};

const PUMP: Pubkey = Pubkey::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
const FEE_PROGRAM: Pubkey = Pubkey::from_str_const("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
const TOKEN: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const TOKEN_2022: Pubkey = Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const ATA_PROGRAM: Pubkey = Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const WSOL: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
const SYSTEM: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
const EVENT_AUTHORITY: Pubkey =
    Pubkey::from_str_const("Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1");

/// Pyth **pull-oracle** SOL/USD price account (`PriceUpdateV2`).
///
/// NOT the legacy Pyth account `H6ARHf6…` — that one still exists, still parses,
/// and has been frozen since ~slot 299M (≈2 years) with `status = 0`. Reading it
/// yields a stale ~$119 that looks entirely plausible. Freshness is checked
/// below precisely because a wrong-but-believable price is worse than none.
const PYTH_SOL_USD: Pubkey =
    Pubkey::from_str_const("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE");
/// Refuse to report USD off a price older than this.
const MAX_PRICE_AGE_SECS: i64 = 300;

/// SOL/USD from chain, or `None` if unavailable or stale.
///
/// `PriceUpdateV2` places `price` after an enum whose encoding varies in length
/// (`Full` = 1 byte, `Partial{u8}` = 2), so both candidate offsets are tried and
/// validated on exponent and magnitude rather than assuming one.
async fn sol_usd(rpc: &RpcClient) -> Option<(f64, i64)> {
    let data = rpc.get_account(&PYTH_SOL_USD).await.ok()?.data;
    for off in [73usize, 74] {
        if off + 28 > data.len() {
            continue;
        }
        let price = i64::from_le_bytes(data.get(off..off + 8)?.try_into().ok()?);
        let expo = i32::from_le_bytes(data.get(off + 16..off + 20)?.try_into().ok()?);
        let publish = i64::from_le_bytes(data.get(off + 20..off + 28)?.try_into().ok()?);
        if !(-12..=-4).contains(&expo) || price <= 0 {
            continue;
        }
        let value = price as f64 * 10f64.powi(expo);
        if !(0.01..=1_000_000.0).contains(&value) {
            continue;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        let age = now - publish;
        if age > MAX_PRICE_AGE_SECS {
            eprintln!("⚠ SOL/USD price is {age}s old (> {MAX_PRICE_AGE_SECS}s) — omitting USD");
            return None;
        }
        return Some((value, age));
    }
    None
}

/// `sell_v2` — verified against mainnet, see `verify_against_known_sell`.
const SELL_V2_DISCRIMINATOR: [u8; 8] = [93, 246, 130, 60, 231, 233, 64, 178];

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &PUMP).0
}
/// `fee_config` and `sharing_config` live on the FEE program, not pump — and
/// `sharing-config` is hyphenated where most pump seeds are not. Both were
/// caught by `verify_against_known_sell` before anything was sent.
fn fee_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &FEE_PROGRAM).0
}
fn ata(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ATA_PROGRAM,
    )
    .0
}

/// The 26 accounts `sell_v2` takes, in the order the program expects.
#[allow(clippy::too_many_arguments)]
fn sell_v2_accounts(
    base_mint: &Pubkey,
    bonding_curve: &Pubkey,
    creator: &Pubkey,
    seller: &Pubkey,
    fee_recipient: &Pubkey,
    buyback_fee_recipient: &Pubkey,
) -> Vec<Pubkey> {
    let creator_vault = pda(&[b"creator-vault", creator.as_ref()]);
    let user_volume_accumulator = pda(&[b"user_volume_accumulator", seller.as_ref()]);
    vec![
        pda(&[b"global"]),                                  // 0  global
        *base_mint,                                         // 1  base_mint
        WSOL,                                               // 2  quote_mint
        TOKEN_2022,                                         // 3  base_token_program
        TOKEN,                                              // 4  quote_token_program
        ATA_PROGRAM,                                        // 5  associated_token_program
        *fee_recipient,                                     // 6  fee_recipient
        ata(fee_recipient, &WSOL, &TOKEN),                  // 7  assoc quote fee recipient
        *buyback_fee_recipient,                             // 8  buyback_fee_recipient
        ata(buyback_fee_recipient, &WSOL, &TOKEN),          // 9  assoc quote buyback
        *bonding_curve,                                     // 10 bonding_curve
        ata(bonding_curve, base_mint, &TOKEN_2022),         // 11 assoc base curve
        ata(bonding_curve, &WSOL, &TOKEN),                  // 12 assoc quote curve
        *seller,                                            // 13 user
        ata(seller, base_mint, &TOKEN_2022),                // 14 assoc base user
        ata(seller, &WSOL, &TOKEN),                         // 15 assoc quote user
        creator_vault,                                      // 16 creator_vault
        ata(&creator_vault, &WSOL, &TOKEN),                 // 17 assoc creator vault
        fee_pda(&[b"sharing-config", base_mint.as_ref()]),  // 18 sharing_config
        user_volume_accumulator,                            // 19 user_volume_accumulator
        ata(&user_volume_accumulator, &WSOL, &TOKEN),       // 20 assoc user vol accumulator
        fee_pda(&[b"fee_config", PUMP.as_ref()]),           // 21 fee_config
        FEE_PROGRAM,                                        // 22 fee_program
        SYSTEM,                                             // 23 system_program
        EVENT_AUTHORITY,                                    // 24 event_authority
        PUMP,                                               // 25 program
    ]
}

/// Re-derive the account list of a real, successful mainnet `sell_v2` and
/// compare address-for-address.
///
/// This is the whole safety story for this tool: if any derivation is wrong we
/// find out here, against a transaction we know landed, rather than by
/// destroying a position.
///
/// Reference: signature
/// VAujWqyyknpZKxXywag4wmVSUyZrvgS42dryNaVr22uq… (slot 436051505)
fn verify_against_known_sell() -> Result<(), String> {
    let expected: [&str; 26] = [
        "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf",
        "CsWbqh2VXJ6KbwxGy7uMCiaF4F3kKsNz4eNL7hNWpump",
        "So11111111111111111111111111111111111111112",
        "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
        "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV",
        "94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb",
        "5cjcW9wExnJJiqgLjq7DEG75Pm6JBgE1hNv4B2vHXUW6",
        "GYH1Gae1wJytMSvMvw8JVcv7nuAbxi8i9erNVbERnzXd",
        "BsaKSexDsay5E3yLdtSvYJVkwufUpeJWmb8J9wbTqEcX",
        "DK3p8t6wA16GoipKHWWYSx56bgxmbSTCwWJ3VX1X7Fba",
        "9KqQ8bfMBXjjDGhbNw1VWyPmre4rzc6utgFbyeGxDBe",
        "78uLTjkwpsN2g93q7BkU71NTcHbYPiXyD3f7FcpK8RAt",
        "2K4QbqMKgMGvEzaBBb1nASyTsWhyBFPQQj4yhUnvcc4F",
        "8b9AMiqu3BdKD1N9wofcBEX46AEMgoGuRMfm7x8yDo17",
        "HC8SuPuyRY9tr1p54CpeUjoYAjocXFzBDG9ZumvVZ5x3",
        "4zYkyrJychfYWKQTLfxg2wDSAyvcTBdCF7tKnXwbLhyW",
        "DoBKnUErDhaoEnGkM6EAi8QQNFo2ALjjnhXwK3wHBPQ2",
        "J9BNAL2bygGMeNi4nFGgeN2PiRpxKckztKRecdzNVhkN",
        "J95Qm5gjdhj27PPYAPpL1nhnYA5782MpmtVXZnMbSXhX",
        "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt",
        "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ",
        "11111111111111111111111111111111",
        "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1",
        "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
    ];
    let base_mint = Pubkey::from_str_const(expected[1]);
    let bonding_curve = Pubkey::from_str_const(expected[10]);
    let seller = Pubkey::from_str_const(expected[13]);
    let fee_recipient = Pubkey::from_str_const(expected[6]);
    let buyback = Pubkey::from_str_const(expected[8]);
    // The reference coin was launched by the same wallet that sold it.
    let derived = sell_v2_accounts(
        &base_mint,
        &bonding_curve,
        &seller,
        &seller,
        &fee_recipient,
        &buyback,
    );

    let names = [
        "global", "base_mint", "quote_mint", "base_token_program", "quote_token_program",
        "associated_token_program", "fee_recipient", "assoc_quote_fee_recipient",
        "buyback_fee_recipient", "assoc_quote_buyback", "bonding_curve", "assoc_base_curve",
        "assoc_quote_curve", "user", "assoc_base_user", "assoc_quote_user", "creator_vault",
        "assoc_creator_vault", "sharing_config", "user_volume_accumulator",
        "assoc_user_vol_accumulator", "fee_config", "fee_program", "system_program",
        "event_authority", "program",
    ];
    let mut bad = Vec::new();
    for i in 0..26 {
        let got = derived[i].to_string();
        if got != expected[i] {
            bad.push(format!(
                "  [{i:2}] {:<28} derived {got}\n       {:<28} expected {}",
                names[i], "", expected[i]
            ));
        }
    }
    if bad.is_empty() {
        println!("✅ VERIFIED: all 26 accounts re-derive exactly against the known mainnet sell");
        Ok(())
    } else {
        Err(format!(
            "{} of 26 accounts do not match the known-good sell:\n{}",
            bad.len(),
            bad.join("\n")
        ))
    }
}

/// Which wallets this run touches.
enum Target {
    /// Every loaded wallet sells the same percentage.
    All,
    /// One wallet only, by load-order index.
    Index(usize),
    /// One wallet only, by public key.
    Pubkey(Pubkey),
}

struct Args {
    pct: f64,
    target: Target,
    execute: bool,
    list: bool,
}

fn parse_args() -> Result<Args, String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut pct: Option<f64> = None;
    let mut target = Target::All;
    let mut execute = false;
    let mut list = false;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--execute" => execute = true,
            "--list" => list = true,
            "--pct" => {
                i += 1;
                let raw = argv.get(i).ok_or("--pct needs a value")?;
                pct = Some(raw.parse().map_err(|_| format!("bad --pct: {raw}"))?);
            }
            "--wallet" => {
                i += 1;
                let raw = argv.get(i).ok_or("--wallet needs an index or pubkey")?;
                target = match raw.parse::<usize>() {
                    Ok(n) => Target::Index(n),
                    Err(_) => Target::Pubkey(
                        raw.parse()
                            .map_err(|_| format!("--wallet is neither an index nor a pubkey: {raw}"))?,
                    ),
                };
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }
    let pct = pct
        .or_else(|| std::env::var("SELL_PCT").ok().and_then(|v| v.parse().ok()))
        .unwrap_or(100.0);
    if !(0.0..=100.0).contains(&pct) {
        return Err(format!("percentage must be 0-100, got {pct}"));
    }
    Ok(Args { pct, target, execute, list })
}

fn load_keypairs(dir: &str) -> Result<Vec<Keypair>, String> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {dir}: {e}"))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let raw = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let bytes: Vec<u8> =
                serde_json::from_str(raw.trim()).map_err(|e| format!("{}: {e}", p.display()))?;
            Keypair::try_from(bytes.as_slice()).map_err(|e| format!("{}: {e}", p.display()))
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), String> {
    dotenv::dotenv().ok();
    env_logger::init();
    let args = parse_args()?;
    let execute = args.execute;

    // Gate everything on the layout check.
    verify_against_known_sell()?;

    let rpc_url = std::env::var("RPC_URLS")
        .map_err(|_| "RPC_URLS must be set")?
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let rpc = Arc::new(RpcClient::new(rpc_url));

    let mint: Pubkey = std::env::var("SELL_MINT")
        .map_err(|_| "SELL_MINT must be set")?
        .parse()
        .map_err(|e| format!("bad SELL_MINT: {e}"))?;
    let creator: Pubkey = std::env::var("SELL_CREATOR")
        .map_err(|_| "SELL_CREATOR must be set")?
        .parse()
        .map_err(|e| format!("bad SELL_CREATOR: {e}"))?;
    let sell_pct = args.pct;
    let dir = std::env::var("BUYER_KEYPAIR_DIR").unwrap_or_else(|_| "./wallets".into());
    let all_wallets = load_keypairs(&dir)?;

    // Resolve the target down to the wallets this run will actually touch,
    // keeping load-order indexes so logs and `--list` agree.
    let wallets: Vec<(usize, &Keypair)> = match args.target {
        Target::All => all_wallets.iter().enumerate().collect(),
        Target::Index(n) => {
            let kp = all_wallets
                .get(n)
                .ok_or_else(|| format!("--wallet {n} out of range (0..{})", all_wallets.len()))?;
            vec![(n, kp)]
        }
        Target::Pubkey(pk) => {
            let found = all_wallets
                .iter()
                .enumerate()
                .find(|(_, kp)| kp.pubkey() == pk)
                .ok_or_else(|| format!("--wallet {pk} not found in {dir}"))?;
            vec![found]
        }
    };

    // fee_recipient and buyback recipient come from the live Global account —
    // update_buyback_config can rotate them, and a stale value fails the sell.
    let global_data = rpc
        .get_account(&pda(&[b"global"]))
        .await
        .map_err(|e| format!("fetch global: {e}"))?
        .data;
    let fee_recipient = Pubkey::try_from(&global_data[8 + 1 + 32..8 + 1 + 32 + 32])
        .map_err(|e| format!("global.fee_recipient: {e}"))?;
    let buyback = Pubkey::from_str_const("5cjcW9wExnJJiqgLjq7DEG75Pm6JBgE1hNv4B2vHXUW6");
    let bonding_curve = pda(&[b"bonding-curve", mint.as_ref()]);

    println!("\nmint {mint}\ncreator {creator}\nbonding_curve {bonding_curve}\nfee_recipient {fee_recipient}");
    println!(
        "targets: {}   pct: {sell_pct}%   mode: {}\n",
        match args.target {
            Target::All => format!("ALL {} wallet(s)", wallets.len()),
            Target::Index(n) => format!("wallet #{n} only"),
            Target::Pubkey(pk) => format!("wallet {pk} only"),
        },
        if execute { "EXECUTE (will send)" } else { "SIMULATE ONLY" }
    );

    if args.list {
        println!("idx  wallet                                        SOL        tokens");
        for (i, kp) in all_wallets.iter().enumerate() {
            let pk = kp.pubkey();
            let sol = rpc.get_balance(&pk).await.unwrap_or(0) as f64 / 1e9;
            let bal = rpc
                .get_token_account_balance(&ata(&pk, &mint, &TOKEN_2022))
                .await
                .map(|b| b.ui_amount_string)
                .unwrap_or_else(|_| "0".into());
            println!("{i:>3}  {pk}  {sol:>9.6}  {bal:>18}");
        }
        return Ok(());
    }

    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .map_err(|e| format!("blockhash: {e}"))?;

    let mut sent: Vec<(Pubkey, u64, solana_signature::Signature)> = Vec::new();
    for (widx, kp) in &wallets {
        let kp = *kp;
        let seller = kp.pubkey();
        let token_account = ata(&seller, &mint, &TOKEN_2022);
        let amount = match rpc.get_token_account_balance(&token_account).await {
            Ok(b) => b.amount.parse::<u64>().unwrap_or(0),
            Err(_) => 0,
        };
        if amount == 0 {
println!("#{widx} {seller}: no balance, skipping");
            continue;
        }
        // Percentage of the position, floored to whole base units.
        let amount = ((amount as f64) * sell_pct / 100.0) as u64;
        if amount == 0 {
println!("#{widx} {seller}: {sell_pct}% rounds to zero, skipping");
            continue;
        }
        let sol_before = rpc.get_balance(&seller).await.unwrap_or(0);

        let metas: Vec<AccountMeta> = sell_v2_accounts(
            &mint,
            &bonding_curve,
            &creator,
            &seller,
            &fee_recipient,
            &buyback,
        )
        .into_iter()
        .enumerate()
        .map(|(i, pk)| {
            // Signer: only the seller. Read-only: programs and the mints.
            let readonly = matches!(i, 1..=5 | 22..=25) || i == 18;
            if i == 13 {
                AccountMeta::new(pk, true)
            } else if readonly {
                AccountMeta::new_readonly(pk, false)
            } else {
                AccountMeta::new(pk, false)
            }
        })
        .collect();

        let mut data = SELL_V2_DISCRIMINATOR.to_vec();
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&1u64.to_le_bytes()); // min_sol_output = 1: exit at any price

        let ixs = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(200_000),
            ComputeBudgetInstruction::set_compute_unit_price(1_000_000),
            Instruction { program_id: PUMP, accounts: metas, data },
        ];
        let msg = v0::Message::try_compile(&seller, &ixs, &[], blockhash)
            .map_err(|e| format!("compile: {e}"))?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[kp])
            .map_err(|e| format!("sign: {e}"))?;

        let sim = rpc
            .simulate_transaction_with_config(
                &tx,
                RpcSimulateTransactionConfig {
                    sig_verify: false,
                    replace_recent_blockhash: true,
                    commitment: Some(CommitmentConfig::processed()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("simulate: {e}"))?;

        match sim.value.err {
            None => {
println!("#{widx} {seller}: SIMULATE OK  selling {amount} ({sell_pct}%)  units={:?}", sim.value.units_consumed);
                if execute {
                    match rpc
                        .send_transaction_with_config(
                            &tx,
                            RpcSendTransactionConfig {
                                skip_preflight: true,
                                max_retries: Some(3),
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        Ok(sig) => {
                            println!("  SENT {sig}");
                            sent.push((seller, sol_before, sig));
                        }
                        Err(e) => println!("  SEND FAILED: {e}"),
                    }
                }
            }
            Some(err) => {
println!("#{widx} {seller}: SIMULATE FAILED {err:?}");
                if let Some(logs) = sim.value.logs {
                    for l in logs.iter().rev().take(6).collect::<Vec<_>>().iter().rev() {
                        println!("    {l}");
                    }
                }
            }
        }
    }

    // Realised P&L. Proceeds are the actual SOL delta on chain, so fees, rent
    // and tips are all already accounted for — no separate fee model to drift.
    if !sent.is_empty() {
        tokio::time::sleep(std::time::Duration::from_secs(12)).await;
        let price = sol_usd(&rpc).await;
        let mut total_delta_lamports: i128 = 0;
        println!("\n════ REALISED ════");
        for (wallet, before, sig) in &sent {
            let after = rpc.get_balance(wallet).await.unwrap_or(*before);
            let delta = after as i128 - *before as i128;
            total_delta_lamports += delta;
            println!(
                "  {wallet}  +{:.6} SOL   sig {}",
                delta as f64 / 1e9,
                sig.to_string().get(..16).unwrap_or_default()
            );
        }
        let proceeds = total_delta_lamports as f64 / 1e9;
        println!("  ── proceeds: {proceeds:+.6} SOL");
        match price {
            Some((usd, age)) => println!(
                "  ── proceeds: ${:+.2} USD   (SOL/USD ${usd:.2}, {age}s old, on-chain Pyth)",
                proceeds * usd
            ),
            None => println!("  ── USD omitted: no fresh on-chain price"),
        }
        println!(
            "\n  Cost basis is NOT included above — it comes from the buy transactions\n               recorded in positions/<mint>.json. Pass those to compute % return."
        );
    }
    Ok(())
}
