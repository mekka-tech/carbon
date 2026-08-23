//! Exit path — `sell_v2`, usable from the interactive console.
//!
//! The account layout is verified against two known-good mainnet sells before
//! the first transaction is built (`verify_layout`). That check has already
//! caught two wrong derivations (`sharing_config` and `fee_config` derive on
//! the FEE program, not pump), so it stays as a hard gate rather than a debug
//! aid.
//!
//! Proceeds arrive as native SOL: pump opens, drains and closes the seller's
//! quote (WSOL) account inside the instruction, so nothing here wraps or
//! unwraps anything.

use {
    crate::pump::pdas,
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
    solana_signature::Signature,
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::{sync::Arc, time::Duration},
};

const SELL_V2_DISCRIMINATOR: [u8; 8] = [93, 246, 130, 60, 231, 233, 64, 178];

/// `sell_v2` takes exactly this many accounts.
pub const SELL_ACCOUNT_COUNT: usize = 26;

/// Index of `user` — the only signer, and the only account we sign for.
const SELLER_INDEX: usize = 13;

/// Pyth **pull-oracle** SOL/USD (`PriceUpdateV2`).
///
/// Not the legacy `H6ARHf6…` account — that still parses but has been frozen
/// ~635 days with `status = 0`, returning a plausible-looking stale price.
const PYTH_SOL_USD: Pubkey = Pubkey::from_str_const("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE");
const MAX_PRICE_AGE_SECS: i64 = 300;

/// A publish time *ahead* of our clock is either clock skew or — far more
/// likely — a mis-parsed offset. The age check has to be bounded on both sides:
/// a one-sided `age > MAX` test reads any far-future garbage as perfectly
/// fresh, which is exactly what a wrong offset produces.
const MAX_PRICE_LEAD_SECS: i64 = 60;

/// Reject a quote whose confidence band is wider than this fraction of the
/// price. The real SOL/USD band runs ~0.06%, so this is nowhere near binding on
/// a correct parse; it exists to reject a wrong one, where `conf` is noise.
const MAX_PRICE_CONF_RATIO: f64 = 0.05;

/// How long to wait for a sell to land before calling it dropped. Matches the
/// dispatcher's landing watcher: past ~30s the blockhash it was signed against
/// has expired, so a transaction that has not landed never will.
// 150 slots at ~400ms is the blockhash lifetime, so a sell can still land
// well past 30s. Polling only that long declared "not sold" for transactions
// that were still live, and — because the in-flight claim is released when this
// returns — invited a second full sell that then landed alongside the first.
/// Compute unit limit for a sell. Sells are not racing anything, so this is
/// sized for headroom rather than for priority.
pub const SELL_COMPUTE_UNIT_LIMIT: u32 = 200_000;
/// Compute unit price for a sell, micro-lamports.
pub const SELL_PRIORITY_MICRO_LAMPORTS: u64 = 1_000_000;
/// Rent-exemption for the quote (WSOL) token account pump opens, drains and
/// closes inside `sell_v2`.
///
/// It is refunded when the account closes at the end of the instruction, but
/// the seller must be able to fund it for the duration — a wallet that cannot
/// is rejected with `InsufficientFundsForRent` no matter how many tokens it
/// holds.
pub const TRANSIENT_QUOTE_ACCOUNT_RENT: u64 = 2_039_280;

/// Lamports a wallet must retain to be able to sell.
///
/// Held back by `dispatch::gas_reserve` at BUY time. A wallet that spends
/// everything on the buy is a trapped position: it holds tokens it cannot
/// exit, and the failure surfaces per-wallet as a raw
/// `InsufficientFundsForRent` from simulation with no hint that the fix is to
/// send it SOL. Sizing the buy to leave this behind is what prevents that.
pub const fn sell_cost_lamports() -> u64 {
    let priority = (SELL_COMPUTE_UNIT_LIMIT as u64).saturating_mul(SELL_PRIORITY_MICRO_LAMPORTS)
        / 1_000_000;
    priority
        .saturating_add(5_000) // base fee
        .saturating_add(TRANSIENT_QUOTE_ACCOUNT_RENT)
        // Headroom: the fee config can move, and a sell that cannot pay is
        // strictly worse than a buy that was slightly smaller.
        .saturating_add(500_000)
}

const CONFIRM_POLLS: usize = 40;
const CONFIRM_POLL_INTERVAL: Duration = Duration::from_secs(2);

fn ata(owner: &Pubkey, mint: &Pubkey, program: &Pubkey) -> Pubkey {
    pdas::associated_token_address_with_program(owner, mint, program)
}

/// The 26 accounts `sell_v2` expects, in program order.
pub fn sell_accounts(
    base_mint: &Pubkey,
    bonding_curve: &Pubkey,
    creator: &Pubkey,
    seller: &Pubkey,
    fee_recipient: &Pubkey,
    buyback: &Pubkey,
) -> Vec<Pubkey> {
    let vault = pdas::creator_vault(creator);
    let uva = pdas::user_volume_accumulator(seller);
    let (t22, tok) = (pdas::TOKEN_2022_PROGRAM_ID, pdas::TOKEN_PROGRAM_ID);
    vec![
        pdas::global(),
        *base_mint,
        pdas::WSOL_MINT,
        t22,
        tok,
        pdas::ASSOCIATED_TOKEN_PROGRAM_ID,
        *fee_recipient,
        ata(fee_recipient, &pdas::WSOL_MINT, &tok),
        *buyback,
        ata(buyback, &pdas::WSOL_MINT, &tok),
        *bonding_curve,
        ata(bonding_curve, base_mint, &t22),
        ata(bonding_curve, &pdas::WSOL_MINT, &tok),
        *seller,
        ata(seller, base_mint, &t22),
        ata(seller, &pdas::WSOL_MINT, &tok),
        vault,
        ata(&vault, &pdas::WSOL_MINT, &tok),
        pdas::sharing_config(base_mint),
        uva,
        ata(&uva, &pdas::WSOL_MINT, &tok),
        pdas::fee_config(),
        pdas::FEE_PROGRAM_ID,
        solana_system_interface::program::ID,
        pdas::event_authority(),
        pdas::PUMPFUN_PROGRAM_ID,
    ]
}

/// The same 26 accounts carrying the signer/writable flags from the on-chain
/// IDL.
///
/// `global` (0) and `fee_config` (21) are **read-only**, matching the v2 buy in
/// `pump/instructions.rs`. The reference mainnet sell declares both writable,
/// but the IDL does not mark them `mut` and the program does not touch them:
/// Anchor accepts an over-declared writable account, so a landed transaction is
/// no evidence it was needed. Over-declaring is not free either — both are
/// singletons shared by every pump trader, and taking a write lock on them
/// serialises our own concurrent sells into separate blocks.
pub fn sell_metas(
    base_mint: &Pubkey,
    bonding_curve: &Pubkey,
    creator: &Pubkey,
    seller: &Pubkey,
    fee_recipient: &Pubkey,
    buyback: &Pubkey,
) -> Vec<AccountMeta> {
    sell_accounts(base_mint, bonding_curve, creator, seller, fee_recipient, buyback)
        .into_iter()
        .enumerate()
        .map(|(i, pk)| {
            if i == SELLER_INDEX {
                AccountMeta::new(pk, true)
            } else if matches!(i, 6..=17 | 19 | 20) {
                AccountMeta::new(pk, false)
            } else {
                AccountMeta::new_readonly(pk, false)
            }
        })
        .collect()
}

/// A known-good mainnet sell where the seller **is** the creator, so slots 16
/// and 19 cannot distinguish a creator-derived PDA from a seller-derived one.
/// `THIRD_PARTY_SELL` covers that; both are checked.
const SELF_SELL: [&str; SELL_ACCOUNT_COUNT] = [
    "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf", // 0  global
    "CsWbqh2VXJ6KbwxGy7uMCiaF4F3kKsNz4eNL7hNWpump", // 1  base_mint
    "So11111111111111111111111111111111111111112",  // 2  quote_mint
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",  // 3  base_token_program
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",  // 4  quote_token_program
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL", // 5  associated_token_program
    "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV", // 6  fee_recipient
    "94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb", // 7  associated_quote_fee_recipient
    "5cjcW9wExnJJiqgLjq7DEG75Pm6JBgE1hNv4B2vHXUW6", // 8  buyback_fee_recipient
    "GYH1Gae1wJytMSvMvw8JVcv7nuAbxi8i9erNVbERnzXd", // 9  associated_quote_buyback_fee_recipient
    "BsaKSexDsay5E3yLdtSvYJVkwufUpeJWmb8J9wbTqEcX", // 10 bonding_curve
    "DK3p8t6wA16GoipKHWWYSx56bgxmbSTCwWJ3VX1X7Fba", // 11 associated_base_bonding_curve
    "9KqQ8bfMBXjjDGhbNw1VWyPmre4rzc6utgFbyeGxDBe",  // 12 associated_quote_bonding_curve
    "78uLTjkwpsN2g93q7BkU71NTcHbYPiXyD3f7FcpK8RAt", // 13 user (== creator here)
    "2K4QbqMKgMGvEzaBBb1nASyTsWhyBFPQQj4yhUnvcc4F", // 14 associated_base_user
    "8b9AMiqu3BdKD1N9wofcBEX46AEMgoGuRMfm7x8yDo17", // 15 associated_quote_user
    "HC8SuPuyRY9tr1p54CpeUjoYAjocXFzBDG9ZumvVZ5x3", // 16 creator_vault
    "4zYkyrJychfYWKQTLfxg2wDSAyvcTBdCF7tKnXwbLhyW", // 17 associated_creator_vault
    "DoBKnUErDhaoEnGkM6EAi8QQNFo2ALjjnhXwK3wHBPQ2", // 18 sharing_config
    "J9BNAL2bygGMeNi4nFGgeN2PiRpxKckztKRecdzNVhkN", // 19 user_volume_accumulator
    "J95Qm5gjdhj27PPYAPpL1nhnYA5782MpmtVXZnMbSXhX", // 20 associated_user_volume_accumulator
    "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt", // 21 fee_config
    "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ",  // 22 fee_program
    "11111111111111111111111111111111",             // 23 system_program
    "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1", // 24 event_authority
    "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",  // 25 program
];

/// A mainnet sell by a wallet that is **not** the creator — the shape the
/// sniper actually sends. Signature
/// 56vKVgQrEB7afe7FD664WtPiWNnW1cp4qX49otm6Zs9ndCzP7hrGz5fKb4RMMAhTrJ9xHBHiuYSTrUGfRAC6uzeJ.
/// Same creator as `SELF_SELL` (a different token by the same dev), so slot 16
/// repeats while slot 19 does not — which is precisely what makes it able to
/// catch a creator/seller mix-up.
const THIRD_PARTY_SELL: [&str; SELL_ACCOUNT_COUNT] = [
    "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf", // 0  global
    "EEAEf5fQgZCyGk2t2dEHz5SxLbL2mzvM7fLSg7m3pump", // 1  base_mint
    "So11111111111111111111111111111111111111112",  // 2  quote_mint
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",  // 3  base_token_program
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",  // 4  quote_token_program
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL", // 5  associated_token_program
    "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV", // 6  fee_recipient
    "94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb", // 7  associated_quote_fee_recipient
    "5cjcW9wExnJJiqgLjq7DEG75Pm6JBgE1hNv4B2vHXUW6", // 8  buyback_fee_recipient
    "GYH1Gae1wJytMSvMvw8JVcv7nuAbxi8i9erNVbERnzXd", // 9  associated_quote_buyback_fee_recipient
    "69ga5pR1yZUosBKntWVgg2966esQMAEPxJ4WccvscYkD", // 10 bonding_curve
    "D59uieqCG7H18Jik6n29LEzqZo3Ly4qDa1PtbqZCWGzV", // 11 associated_base_bonding_curve
    "vmLfZMt41xfKb6gR4bv4BvECnb5kM2yfQz7xyQFCXfi",  // 12 associated_quote_bonding_curve
    "3xhR7hBgC2irmfC1t8L51xjxJ5co3W9vDdbNhPqWDBdJ", // 13 user (!= creator)
    "8ZAEgzNqfHBVfi5vWx6CCQiknyy8rfu529zZ2w2ajeAa", // 14 associated_base_user
    "67o8sroksbGQdjBCKoSjxj99qHFaeBtFwAsNhU2rqhxW", // 15 associated_quote_user
    "HC8SuPuyRY9tr1p54CpeUjoYAjocXFzBDG9ZumvVZ5x3", // 16 creator_vault
    "4zYkyrJychfYWKQTLfxg2wDSAyvcTBdCF7tKnXwbLhyW", // 17 associated_creator_vault
    "9t1eVLWya9E6Mq8ywDe64zJLFK52rHK6EtAWPZmXoJyy", // 18 sharing_config
    "7bTioAjN1Up2wkVHxk6LVbtxxpZC2BTy3s1rf8zzmMqQ", // 19 user_volume_accumulator
    "AwVB2QV89jcUhcDdvYeRJCLKnPPhgQWjwcA2aUDovgjw", // 20 associated_user_volume_accumulator
    "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt", // 21 fee_config
    "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ",  // 22 fee_program
    "11111111111111111111111111111111",             // 23 system_program
    "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1", // 24 event_authority
    "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",  // 25 program
];

/// The creator behind both fixtures, read from the `BondingCurve` account of
/// each mint. In `SELF_SELL` it is also the seller; in `THIRD_PARTY_SELL` it is
/// not.
const FIXTURE_CREATOR: &str = "78uLTjkwpsN2g93q7BkU71NTcHbYPiXyD3f7FcpK8RAt";

/// Re-derive one fixture and compare address-for-address.
///
/// Mint, curve, fee recipient and buyback are read back out of the fixture
/// itself (slots 1, 10, 6, 8); only creator and seller are supplied, since
/// those are the two inputs a mix-up would swap.
fn compare_layout(
    label: &str,
    expected: &[&str; SELL_ACCOUNT_COUNT],
    creator: &Pubkey,
    seller: &Pubkey,
) -> Result<(), String> {
    let got = sell_accounts(
        &Pubkey::from_str_const(expected[1]),
        &Pubkey::from_str_const(expected[10]),
        creator,
        seller,
        &Pubkey::from_str_const(expected[6]),
        &Pubkey::from_str_const(expected[8]),
    );
    let bad: Vec<String> = (0..SELL_ACCOUNT_COUNT)
        .filter(|i| got[*i].to_string() != expected[*i])
        .map(|i| format!("  [{i}] derived {} expected {}", got[i], expected[i]))
        .collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "sell layout mismatch in {label} ({} of {SELL_ACCOUNT_COUNT}):\n{}",
            bad.len(),
            bad.join("\n")
        ))
    }
}

/// Re-derive both known-good mainnet sells and compare address-for-address.
pub fn verify_layout() -> Result<(), String> {
    let creator = Pubkey::from_str_const(FIXTURE_CREATOR);
    compare_layout(
        "self-sell",
        &SELF_SELL,
        &creator,
        &Pubkey::from_str_const(SELF_SELL[SELLER_INDEX]),
    )?;
    compare_layout(
        "third-party sell",
        &THIRD_PARTY_SELL,
        &creator,
        &Pubkey::from_str_const(THIRD_PARTY_SELL[SELLER_INDEX]),
    )
}

/// `N` little-endian bytes at `off`, `None` when they do not fit.
fn le_bytes<const N: usize>(data: &[u8], off: usize) -> Option<[u8; N]> {
    let end = off.checked_add(N)?;
    data.get(off..end)?.try_into().ok()
}

/// SOL/USD from chain, `None` when unavailable, implausible or stale.
pub async fn sol_usd(rpc: &RpcClient) -> Option<f64> {
    let data = rpc.get_account(&PYTH_SOL_USD).await.ok()?.data;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    // `price` follows a variable-length enum, so both offsets are tried and
    // validated rather than assuming one. Every rejection below must `continue`
    // and never `return`: bailing out on the first offset means 74 is only ever
    // reached when 73 fails to *parse*, so a stale-looking 73 would mask a good
    // 74 entirely.
    for off in [73usize, 74] {
        let (Some(price), Some(conf), Some(expo), Some(publish)) = (
            le_bytes(&data, off).map(i64::from_le_bytes),
            le_bytes(&data, off.saturating_add(8)).map(u64::from_le_bytes),
            le_bytes(&data, off.saturating_add(16)).map(i32::from_le_bytes),
            le_bytes(&data, off.saturating_add(20)).map(i64::from_le_bytes),
        ) else {
            continue;
        };
        if !(-12..=-4).contains(&expo) || price <= 0 {
            continue;
        }
        let scale = 10f64.powi(expo);
        let value = price as f64 * scale;
        if !(0.01..=1_000_000.0).contains(&value) {
            continue;
        }
        // A confidence band this wide is not market uncertainty, it is a wrong
        // offset: `conf` is the field most likely to read as noise.
        if conf as f64 * scale > value * MAX_PRICE_CONF_RATIO {
            log::warn!("SOL/USD at offset {off} has an implausible confidence band — skipping");
            continue;
        }
        // Bounded both ways. Negative age means the publish time is in our
        // future, which a correct parse never is.
        let age = now.saturating_sub(publish);
        if age > MAX_PRICE_AGE_SECS {
            log::warn!("SOL/USD at offset {off} is {age}s old — skipping");
            continue;
        }
        if age < -MAX_PRICE_LEAD_SECS {
            log::warn!("SOL/USD at offset {off} is timestamped in the future — skipping");
            continue;
        }
        return Some(value);
    }
    log::warn!("no plausible SOL/USD price in the Pyth account — omitting USD");
    None
}

pub struct SellOutcome {
    pub wallet: Pubkey,
    pub index: usize,
    pub tokens: u64,
    pub signature: Option<String>,
    pub error: Option<String>,
    /// Payer balance immediately before the send, so proceeds can be measured
    /// out of band without holding up the caller. `None` when the pre-send
    /// balance could not be read — there is then no baseline, and no realised
    /// figure can be computed at all.
    pub balance_before: Option<u64>,
}

/// Tokens to sell for `pct` of `held`.
///
/// `pct >= 100` short-circuits to exactly `held`. The float path can round
/// *above* the balance (`held as f64` is lossy past 2^53, and `* pct / 100.0`
/// rounds), and a sell for more than the balance is rejected outright — `s 100
/// go` is the one command that must never fail that way. Everything below 100%
/// goes through integer math in basis points so the only float step is the
/// percentage itself.
fn tokens_for_pct(held: u64, pct: f64) -> u64 {
    if pct >= 100.0 {
        return held;
    }
    if !pct.is_finite() || pct <= 0.0 {
        return 0;
    }
    let bps = (pct * 100.0) as u128;
    let amount = u128::from(held)
        .saturating_mul(bps)
        .checked_div(10_000)
        .unwrap_or(0);
    // `bps < 10_000` here so this cannot exceed `held`, but clamp rather than
    // depend on that.
    u64::try_from(amount).unwrap_or(held).min(held)
}

/// Sell `pct` of one wallet's position. Simulates first and only sends when
/// `execute` and the simulation succeeded.
#[allow(clippy::too_many_arguments)]
pub async fn sell_one(
    rpc: &Arc<RpcClient>,
    kp: &Keypair,
    index: usize,
    mint: &Pubkey,
    bonding_curve: &Pubkey,
    creator: &Pubkey,
    fee_recipient: &Pubkey,
    buyback: &Pubkey,
    pct: f64,
    execute: bool,
    blockhash: solana_hash::Hash,
) -> SellOutcome {
    let seller = kp.pubkey();
    let mut out = SellOutcome {
        wallet: seller,
        index,
        tokens: 0,
        signature: None,
        error: None,
        balance_before: None,
    };
    // v2 coins are Token-2022; v1 (`create`) coins are classic SPL Token, and
    // the sniper buys both. Deriving Token-2022 unconditionally meant a v1
    // position reported "no balance" on every wallet, was marked exited by the
    // background scan, and had NO exit path at all — the tokens were held and
    // unsellable, with the panel showing zero.
    //
    // Resolved from the mint's owning program rather than from the position
    // record, because that is a fact of the chain rather than of a file an
    // older build may have written.
    let token_program = match rpc.get_account(mint).await {
        Ok(acct)
            if acct.owner == pdas::TOKEN_PROGRAM_ID
                || acct.owner == pdas::TOKEN_2022_PROGRAM_ID =>
        {
            acct.owner
        }
        // Unreadable mint: assume v2, which is what the sniper overwhelmingly
        // holds, and let the balance read report honestly if that is wrong.
        _ => pdas::TOKEN_2022_PROGRAM_ID,
    };
    let token_account = ata(&seller, mint, &token_program);
    let held = match rpc.get_token_account_balance(&token_account).await {
        Ok(b) => b.amount.parse::<u64>().unwrap_or(0),
        Err(_) => 0,
    };
    let amount = tokens_for_pct(held, pct);
    if amount == 0 {
        out.error = Some(if held == 0 {
            "no balance".into()
        } else {
            format!("{pct}% of {held} rounds to zero")
        });
        return out;
    }
    out.tokens = amount;

    let metas = sell_metas(mint, bonding_curve, creator, &seller, fee_recipient, buyback);

    let mut data = SELL_V2_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&amount.to_le_bytes());
    // min_sol_output = 1: exit at any price. A floor is meaningless on an exit
    // we have already decided to take, and a wrong one only blocks it.
    data.extend_from_slice(&1u64.to_le_bytes());

    let ixs = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(SELL_COMPUTE_UNIT_LIMIT),
        ComputeBudgetInstruction::set_compute_unit_price(SELL_PRIORITY_MICRO_LAMPORTS),
        Instruction {
            program_id: pdas::PUMPFUN_PROGRAM_ID,
            accounts: metas,
            data,
        },
    ];
    let msg = match v0::Message::try_compile(&seller, &ixs, &[], blockhash) {
        Ok(m) => m,
        Err(e) => {
            out.error = Some(format!("compile: {e}"));
            return out;
        }
    };
    let tx = match VersionedTransaction::try_new(VersionedMessage::V0(msg), &[kp]) {
        Ok(t) => t,
        Err(e) => {
            out.error = Some(format!("sign: {e}"));
            return out;
        }
    };

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
        .await;
    match sim {
        Ok(r) if r.value.err.is_some() => {
            out.error = Some(format!("simulate: {:?}", r.value.err));
            return out;
        }
        Err(e) => {
            out.error = Some(format!("simulate rpc: {e}"));
            return out;
        }
        _ => {}
    }
    if !execute {
        return out;
    }

    // `ok()`, not `unwrap_or(0)`. A failed balance read used to leave a zero
    // baseline, and `after - 0` is the entire wallet — reported as realised
    // proceeds from this one sell. An absent baseline has to stay absent.
    let before = rpc.get_balance(&seller).await.ok();
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
            out.signature = Some(sig.to_string());
            // Deliberately NOT waited on here. Measuring proceeds means waiting
            // for confirmation; doing that inline made a 4-wallet sell take 45
            // seconds and froze the panel. The caller spawns `report_proceeds`
            // instead, which logs the realised figure into the events panel
            // when it lands.
            out.balance_before = before;
        }
        Err(e) => out.error = Some(format!("send: {e}")),
    }
    out
}

/// What became of a sent sell.
enum Landing {
    Confirmed { slot: u64 },
    /// Landed, but the program rejected it. No proceeds.
    Failed(String),
    /// Never observed on chain within the blockhash's lifetime.
    Dropped,
}

/// Poll until the signature is seen on chain, or the blockhash has expired.
///
/// Sends go out with `skip_preflight`, so nothing upstream tells us a
/// transaction was dropped — only its absence here does.
async fn await_landing(rpc: &RpcClient, sig: &Signature) -> Landing {
    for _ in 0..CONFIRM_POLLS {
        tokio::time::sleep(CONFIRM_POLL_INTERVAL).await;
        let Ok(statuses) = rpc.get_signature_statuses(std::slice::from_ref(sig)).await else {
            continue;
        };
        let Some(Some(status)) = statuses.value.first() else {
            continue;
        };
        return match &status.err {
            Some(err) => Landing::Failed(format!("{err:?}")),
            None => Landing::Confirmed { slot: status.slot },
        };
    }
    Landing::Dropped
}

/// Wait for a sell to confirm and log the realised SOL/USD delta.
///
/// Runs detached so a sell returns immediately and the panel keeps updating.
/// Every path that cannot produce a trustworthy figure says so rather than
/// reporting a number: a dropped transaction and a genuinely zero-proceeds sell
/// both diff to ~0 lamports, and the two must never read the same in the log.
pub async fn report_proceeds(
    rpc: Arc<RpcClient>,
    wallet: Pubkey,
    index: usize,
    before: Option<u64>,
    signature: String,
) {
    let short = signature.get(..16).unwrap_or(&signature).to_owned();
    let Ok(sig) = signature.parse::<Signature>() else {
        log::warn!("#{index} unparseable signature {short} — cannot confirm the sell");
        return;
    };

    let slot = match await_landing(&rpc, &sig).await {
        Landing::Confirmed { slot } => slot,
        Landing::Failed(err) => {
            log::warn!("#{index} SELL FAILED on chain: {err}  sig {short}");
            return;
        }
        Landing::Dropped => {
            log::warn!(
                "#{index} SELL UNCONFIRMED after {}s — no status seen. The blockhash has \
                 expired by now so it should not land, but VERIFY the balance before selling \
                 this wallet again  sig {short}",
                CONFIRM_POLLS.saturating_mul(CONFIRM_POLL_INTERVAL.as_secs() as usize)
            );
            return;
        }
    };

    let Some(before) = before else {
        log::warn!(
            "#{index} SOLD in slot {slot} but the pre-send balance was unavailable — realised amount unknown  sig {short}"
        );
        return;
    };
    let Ok(after) = rpc.get_balance(&wallet).await else {
        log::warn!(
            "#{index} SOLD in slot {slot} but the post-send balance was unavailable — realised amount unknown  sig {short}"
        );
        return;
    };

    // Net of the transaction fee and the priority fee, which is what actually
    // hit the wallet.
    let delta = i128::from(after).saturating_sub(i128::from(before));
    let sol = delta as f64 / 1e9;
    match sol_usd(&rpc).await {
        Some(usd) => log::info!(
            "REALISED #{index} {:+.6} SOL (${:+.2}) in slot {slot}  sig {short}",
            sol,
            sol * usd
        ),
        None => log::info!("REALISED #{index} {sol:+.6} SOL in slot {slot}  sig {short}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (is_signer, is_writable) per the on-chain pump IDL for `sell_v2`.
    /// `global` (0) and `fee_config` (21) are read-only there, exactly as in
    /// the v2 buy — see `sell_metas`.
    const FIXTURE_FLAGS: [(bool, bool); SELL_ACCOUNT_COUNT] = [
        (false, false), // 0  global
        (false, false), // 1  base_mint
        (false, false), // 2  quote_mint
        (false, false), // 3  base_token_program
        (false, false), // 4  quote_token_program
        (false, false), // 5  associated_token_program
        (false, true),  // 6  fee_recipient
        (false, true),  // 7  associated_quote_fee_recipient
        (false, true),  // 8  buyback_fee_recipient
        (false, true),  // 9  associated_quote_buyback_fee_recipient
        (false, true),  // 10 bonding_curve
        (false, true),  // 11 associated_base_bonding_curve
        (false, true),  // 12 associated_quote_bonding_curve
        (true, true),   // 13 user
        (false, true),  // 14 associated_base_user
        (false, true),  // 15 associated_quote_user
        (false, true),  // 16 creator_vault
        (false, true),  // 17 associated_creator_vault
        (false, false), // 18 sharing_config
        (false, true),  // 19 user_volume_accumulator
        (false, true),  // 20 associated_user_volume_accumulator
        (false, false), // 21 fee_config
        (false, false), // 22 fee_program
        (false, false), // 23 system_program
        (false, false), // 24 event_authority
        (false, false), // 25 program
    ];

    fn fixture_metas(expected: &[&str; SELL_ACCOUNT_COUNT]) -> Vec<AccountMeta> {
        sell_metas(
            &Pubkey::from_str_const(expected[1]),
            &Pubkey::from_str_const(expected[10]),
            &Pubkey::from_str_const(FIXTURE_CREATOR),
            &Pubkey::from_str_const(expected[SELLER_INDEX]),
            &Pubkey::from_str_const(expected[6]),
            &Pubkey::from_str_const(expected[8]),
        )
    }

    #[test]
    fn sell_layout_matches_known_mainnet_sells() {
        // The gate that caught sharing_config and fee_config deriving on the
        // wrong program. If pump changes the layout this fails loudly here
        // rather than by sending a malformed transaction.
        verify_layout().expect("sell account layout must match mainnet");
    }

    #[test]
    fn sell_takes_exactly_26_accounts() {
        let k = Pubkey::new_unique();
        assert_eq!(
            sell_accounts(&k, &k, &k, &k, &k, &k).len(),
            SELL_ACCOUNT_COUNT
        );
    }

    #[test]
    fn sell_account_flags_match_the_idl() {
        let metas = fixture_metas(&THIRD_PARTY_SELL);
        assert_eq!(metas.len(), SELL_ACCOUNT_COUNT);
        for (i, (signer, writable)) in FIXTURE_FLAGS.iter().enumerate() {
            assert_eq!(metas[i].is_signer, *signer, "signer flag {i}");
            assert_eq!(metas[i].is_writable, *writable, "writable flag {i}");
        }
        // Exactly one signer, and it is the seller.
        let signers: Vec<_> = metas.iter().filter(|a| a.is_signer).collect();
        assert_eq!(signers.len(), 1, "exactly one signer");
        assert_eq!(
            signers[0].pubkey,
            Pubkey::from_str_const(THIRD_PARTY_SELL[SELLER_INDEX]),
            "the signer is the seller"
        );
    }

    #[test]
    fn the_two_shared_singletons_stay_read_only() {
        // Regression: both were sent writable, which takes a write lock on two
        // accounts every pump trader touches and serialises our own concurrent
        // sells into separate blocks.
        let metas = fixture_metas(&THIRD_PARTY_SELL);
        assert!(!metas[0].is_writable, "global must be read-only");
        assert!(!metas[21].is_writable, "fee_config must be read-only");
    }

    #[test]
    fn creator_and_seller_slots_derive_from_the_right_key() {
        // The self-sell fixture passes one key as both creator and seller, so
        // it cannot tell slot 16 (creator-derived) from slot 19
        // (seller-derived). This one can: creator != seller.
        let creator = Pubkey::from_str_const(FIXTURE_CREATOR);
        let seller = Pubkey::from_str_const(THIRD_PARTY_SELL[SELLER_INDEX]);
        assert_ne!(creator, seller, "fixture must not be a self-sell");

        let got = sell_accounts(
            &Pubkey::from_str_const(THIRD_PARTY_SELL[1]),
            &Pubkey::from_str_const(THIRD_PARTY_SELL[10]),
            &creator,
            &seller,
            &Pubkey::from_str_const(THIRD_PARTY_SELL[6]),
            &Pubkey::from_str_const(THIRD_PARTY_SELL[8]),
        );

        // creator_vault and its quote ATA come from the CREATOR.
        assert_eq!(got[16], pdas::creator_vault(&creator), "slot 16 vs mainnet");
        assert_eq!(got[16], Pubkey::from_str_const(THIRD_PARTY_SELL[16]));
        assert_ne!(
            got[16],
            pdas::creator_vault(&seller),
            "slot 16 must not derive from the seller"
        );
        assert_eq!(
            got[17],
            ata(&got[16], &pdas::WSOL_MINT, &pdas::TOKEN_PROGRAM_ID)
        );

        // user_volume_accumulator and its quote ATA come from the SELLER.
        assert_eq!(
            got[19],
            pdas::user_volume_accumulator(&seller),
            "slot 19 vs mainnet"
        );
        assert_eq!(got[19], Pubkey::from_str_const(THIRD_PARTY_SELL[19]));
        assert_ne!(
            got[19],
            pdas::user_volume_accumulator(&creator),
            "slot 19 must not derive from the creator"
        );
        assert_eq!(
            got[20],
            ata(&got[19], &pdas::WSOL_MINT, &pdas::TOKEN_PROGRAM_ID)
        );

        // Swapping the two inputs must be caught, not silently accepted.
        assert!(
            compare_layout("swapped", &THIRD_PARTY_SELL, &seller, &creator).is_err(),
            "a creator/seller swap must fail verification"
        );
    }

    /// A balance whose `f64` image rounds *up*: past 2^53 only even integers
    /// are representable, and 2^53+3 is nearer 2^53+4 than 2^53+2. Pump
    /// supplies sit in this range (1e9 tokens at 6 decimals), so it is not a
    /// contrived value.
    const HELD_THAT_ROUNDS_UP: u64 = 9_007_199_254_740_995;

    #[test]
    fn full_exit_never_asks_for_more_than_is_held() {
        for held in [
            1u64,
            3,
            999,
            1_000_000_007,
            HELD_THAT_ROUNDS_UP,
            u64::MAX / 3,
            u64::MAX,
        ] {
            assert_eq!(tokens_for_pct(held, 100.0), held, "100% of {held}");
            // Anything above 100 is still capped at the balance.
            assert_eq!(tokens_for_pct(held, 100.5), held);
            assert!(tokens_for_pct(held, 99.999) <= held, "99.999% of {held}");
            assert!(tokens_for_pct(held, 50.0) <= held);
        }
    }

    #[test]
    fn the_float_path_it_replaced_really_did_overshoot() {
        // Guards the reason for the short-circuit rather than the fix: the old
        // `(held as f64) * pct / 100.0` asks the program for more tokens than
        // the account holds, and the sell is rejected — on `s 100 go`, the one
        // command that most needs to work.
        let held = HELD_THAT_ROUNDS_UP;
        let old = ((held as f64) * 100.0 / 100.0) as u64;
        assert!(old > held, "fixture must actually demonstrate the overshoot");
        assert_eq!(tokens_for_pct(held, 100.0), held);
    }

    #[test]
    fn partial_exits_use_integer_math() {
        assert_eq!(tokens_for_pct(1_000, 50.0), 500);
        assert_eq!(tokens_for_pct(1_000, 25.5), 255);
        assert_eq!(tokens_for_pct(1_000, 0.05), 0); // rounds to zero, reported
        assert_eq!(tokens_for_pct(1_000_000, 0.05), 500);
        assert_eq!(tokens_for_pct(0, 100.0), 0);
        assert_eq!(tokens_for_pct(100, 0.0), 0);
        // Nonsense percentages yield nothing rather than a wild amount.
        assert_eq!(tokens_for_pct(100, -5.0), 0);
        assert_eq!(tokens_for_pct(100, f64::NAN), 0);
        assert_eq!(tokens_for_pct(100, f64::NEG_INFINITY), 0);
        assert_eq!(tokens_for_pct(100, f64::INFINITY), 100);
    }
}
