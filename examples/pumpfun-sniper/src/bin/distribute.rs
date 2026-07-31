//! Fund the sniper wallets from a single source wallet.
//!
//! ```text
//!   distribute --list                  # wallets, balances, what each needs
//!   distribute                         # print the plan, send nothing
//!   distribute --execute               # actually send
//!   distribute --target 0.2 --execute  # top every wallet up to 0.2 SOL
//!   distribute --max-wallets 3 --execute  # fund 3 of them, rest next run
//! ```
//!
//! # What this does and does not hide
//!
//! Thirty wallets buying one mint inside two blocks is a stronger fingerprint
//! than any funding graph, and no amount of care here erases it. What funding
//! hygiene actually buys is unlinking the *cluster* from the source wallet, so
//! the capital base and the history across launches stay private. That is a
//! real goal, and a narrower one than "private distribution".
//!
//! Three rules follow from it, and they are the reason this is a program
//! rather than a loop around `solana transfer`:
//!
//! 1. **One transfer per transaction.** A single transaction carrying thirty
//!    `SystemProgram::Transfer` instructions into thirty fresh addresses is
//!    the most identifiable object that could be put on chain — it labels the
//!    whole set as one entity in one instruction list, permanently.
//! 2. **Amounts must not repeat.** Thirty transfers of exactly 0.15 SOL
//!    cluster just as well as one batched transaction. Amounts are jittered
//!    per wallet from the OS entropy pool.
//! 3. **Order and timing must not correlate.** Sends are shuffled out of index
//!    order and spaced by a random delay, so the sequence on chain carries no
//!    information about which wallet is which.
//!
//! None of this defeats a determined graph analysis: every lamport still
//! traces back to the source in one hop. Breaking that link needs a route
//! through something with a real anonymity set. `--deposits` is the seam for
//! it: give this tool a list of destination addresses from a privacy provider
//! and it becomes the sender for that flow, with the same three rules and the
//! same safety checks. See `docs/FUNDING.md`.
//!
//! Reads `RPC_URLS`, `BUYER_KEYPAIR_DIR` and `FUNDING_KEYPAIR` from the
//! environment / `.env`.

use {
    solana_client::{nonblocking::rpc_client::RpcClient, rpc_config::RpcSendTransactionConfig},
    solana_commitment_config::CommitmentConfig,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_keypair::Keypair,
    solana_message::{v0, VersionedMessage},
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::{str::FromStr, sync::Arc},
};

/// Default top-up target per wallet.
const DEFAULT_TARGET_SOL: f64 = 0.15;
/// Amounts land within ±this percentage of the target.
const DEFAULT_JITTER_PCT: f64 = 12.0;
/// Delay range between sends, milliseconds.
const DEFAULT_MIN_DELAY_MS: u64 = 800;
const DEFAULT_MAX_DELAY_MS: u64 = 4_000;
/// Modest priority fee. This is not a race — it is a funding run, and paying
/// snipe-grade fees on thirty transfers is money set on fire.
const PRIORITY_FEE_MICRO_LAMPORTS: u64 = 50_000;
const COMPUTE_UNIT_LIMIT: u32 = 450;
/// Lamports left in the source wallet, so it can still pay fees afterwards.
const SOURCE_RESERVE_LAMPORTS: u64 = 10_000_000;
/// A wallet within this much of its target is left alone, so re-running is
/// cheap and idempotent rather than a second full round of transfers.
const TOP_UP_DUST_LAMPORTS: u64 = 1_000_000;

/// One funding transfer.
#[derive(Debug, Clone)]
struct Transfer {
    label: String,
    to: Pubkey,
    lamports: u64,
    /// What the wallet already holds. `None` for a destination that is not one
    /// of ours (a provider deposit address), where topping up is meaningless.
    current: Option<u64>,
}

/// Random bytes from the OS.
///
/// Jitter that an observer can predict is decorative — it has to come from a
/// real entropy source, not from a seeded PRNG or the clock. `/dev/urandom` is
/// that source on every platform this runs on, and costs no dependency.
fn os_random(buf: &mut [u8]) -> Result<(), String> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom")
        .map_err(|e| format!("cannot open /dev/urandom for jitter: {e}"))?;
    f.read_exact(buf)
        .map_err(|e| format!("cannot read /dev/urandom: {e}"))
}

fn random_u64() -> Result<u64, String> {
    let mut b = [0u8; 8];
    os_random(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// Uniform in `[lo, hi]`. Returns `lo` when the range is empty or inverted,
/// which keeps a misconfigured delay range from panicking a funding run.
fn random_in(lo: u64, hi: u64) -> Result<u64, String> {
    if hi <= lo {
        return Ok(lo);
    }
    let span = hi.saturating_sub(lo).saturating_add(1);
    Ok(lo.saturating_add(random_u64()? % span))
}

/// Target amount with ±`jitter_pct` applied, uniform across the whole band.
///
/// Written as a single draw over `[low, high]`. An earlier version composed it
/// out of a `saturating_sub` against the band's half-width, which collapsed
/// every draw in the lower half to exactly `target`: half the wallets received
/// byte-identical amounts, which is the precise fingerprint this exists to
/// remove. A band check passes on that bug — the distribution is what has to
/// be asserted, so the tests below assert it.
fn jittered_lamports(target: u64, jitter_pct: f64) -> Result<u64, String> {
    if jitter_pct <= 0.0 || target == 0 {
        return Ok(target);
    }
    // Basis points, so the whole computation stays in integers.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let span_bps = (jitter_pct * 100.0).min(9_000.0) as u64;
    let half = target.saturating_mul(span_bps).saturating_div(10_000);
    random_in(target.saturating_sub(half), target.saturating_add(half))
}

/// Fisher-Yates over OS entropy, so send order carries no index information.
fn shuffle<T>(items: &mut [T]) -> Result<(), String> {
    if items.len() < 2 {
        return Ok(());
    }
    let mut i = items.len().saturating_sub(1);
    while i > 0 {
        let j = usize::try_from(random_u64()? % (u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1)))
            .unwrap_or(0);
        items.swap(i, j);
        i = i.saturating_sub(1);
    }
    Ok(())
}

fn lamports_to_sol(lamports: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let sol = lamports as f64 / 1e9;
    sol
}

fn sol_to_lamports(sol: f64) -> u64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lamports = (sol * 1e9) as u64;
    lamports
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Source wallet: a keypair file path, or a base58 secret.
fn load_funding_keypair() -> Result<Keypair, String> {
    if let Ok(path) = std::env::var("FUNDING_KEYPAIR") {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read FUNDING_KEYPAIR {path}: {e}"))?;
        let bytes: Vec<u8> = serde_json::from_str(raw.trim())
            .map_err(|e| format!("invalid keypair JSON in {path}: {e}"))?;
        return Keypair::try_from(bytes.as_slice())
            .map_err(|e| format!("invalid keypair in {path}: {e}"));
    }
    if let Ok(b58) = std::env::var("FUNDING_KEYPAIR_B58") {
        let bytes = bs58::decode(b58.trim())
            .into_vec()
            .map_err(|e| format!("FUNDING_KEYPAIR_B58 is not base58: {e}"))?;
        return Keypair::try_from(bytes.as_slice())
            .map_err(|e| format!("FUNDING_KEYPAIR_B58 is not a keypair: {e}"));
    }
    Err("set FUNDING_KEYPAIR=/path/to/source.json (or FUNDING_KEYPAIR_B58)".into())
}

/// Destination wallets, in the sniper's own load order so the indices printed
/// here match the ones `s`/`b` take in the console.
fn load_destinations() -> Result<Vec<(String, Pubkey)>, String> {
    let dir = std::env::var("BUYER_KEYPAIR_DIR")
        .map_err(|_| "set BUYER_KEYPAIR_DIR to the wallets directory".to_string())?;
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| format!("cannot read {dir}: {e}"))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let raw = std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            let bytes: Vec<u8> = serde_json::from_str(raw.trim())
                .map_err(|e| format!("invalid keypair JSON in {}: {e}", p.display()))?;
            let kp = Keypair::try_from(bytes.as_slice())
                .map_err(|e| format!("invalid keypair in {}: {e}", p.display()))?;
            let label = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("wallet")
                .to_string();
            Ok((label, kp.pubkey()))
        })
        .collect()
}

/// Destinations supplied by a privacy provider: `address[,amount_sol]` per
/// line. Amounts are taken verbatim when present — a provider quote is exact,
/// and jittering it would land outside the quoted band and fail the swap.
fn load_deposit_file(path: &str) -> Result<Vec<Transfer>, String> {
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(',').map(str::trim);
        let addr = parts.next().unwrap_or_default();
        let to = Pubkey::from_str(addr)
            .map_err(|_| format!("{path}:{}: not a pubkey: {addr}", i.saturating_add(1)))?;
        let lamports = match parts.next() {
            Some(a) if !a.is_empty() => sol_to_lamports(
                a.parse::<f64>()
                    .map_err(|_| format!("{path}:{}: bad amount: {a}", i.saturating_add(1)))?,
            ),
            _ => 0,
        };
        out.push(Transfer {
            label: format!("deposit-{}", i.saturating_add(1)),
            to,
            lamports,
            current: None,
        });
    }
    if out.is_empty() {
        return Err(format!("{path} lists no destinations"));
    }
    Ok(out)
}

async fn send_one(
    rpc: &Arc<RpcClient>,
    from: &Keypair,
    transfer: &Transfer,
    blockhash: solana_hash::Hash,
) -> Result<String, String> {
    let ixs = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_UNIT_LIMIT),
        ComputeBudgetInstruction::set_compute_unit_price(PRIORITY_FEE_MICRO_LAMPORTS),
        solana_system_interface::instruction::transfer(
            &from.pubkey(),
            &transfer.to,
            transfer.lamports,
        ),
    ];
    let message = v0::Message::try_compile(&from.pubkey(), &ixs, &[], blockhash)
        .map_err(|e| format!("compile: {e}"))?;
    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[from])
        .map_err(|e| format!("sign: {e}"))?;
    rpc.send_transaction_with_config(
        &tx,
        RpcSendTransactionConfig {
            skip_preflight: false,
            ..Default::default()
        },
    )
    .await
    .map(|sig| sig.to_string())
    .map_err(|e| format!("send: {e}"))
}

#[tokio::main]
async fn main() -> Result<(), String> {
    dotenv::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let value_of = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i.saturating_add(1)))
            .cloned()
    };

    let execute = has("--execute");
    let list_only = has("--list");
    // Fund at most this many wallets per run. Spreading thirty transfers over
    // days is done by running this repeatedly from a scheduler, not by holding
    // a process open for the whole window: a 58-hour foreground run dies to a
    // dropped SSH session, a reboot, or an OOM, and resumes nothing.
    // Idempotency (amounts come from live balances) is what makes that safe.
    let max_wallets: Option<usize> = value_of("--max-wallets").and_then(|v| v.parse().ok());
    let target_sol = value_of("--target")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| env_f64("DIST_TARGET_SOL", DEFAULT_TARGET_SOL));
    let jitter_pct = env_f64("DIST_JITTER_PCT", DEFAULT_JITTER_PCT);
    let min_delay = env_u64("DIST_MIN_DELAY_MS", DEFAULT_MIN_DELAY_MS);
    let max_delay = env_u64("DIST_MAX_DELAY_MS", DEFAULT_MAX_DELAY_MS);

    let rpc_url = std::env::var("RPC_URLS")
        .map_err(|_| "set RPC_URLS".to_string())?
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let rpc = Arc::new(RpcClient::new_with_commitment(
        rpc_url,
        CommitmentConfig::confirmed(),
    ));

    // Build the transfer list. Either our own wallets (top up to target) or a
    // provider's deposit addresses (amounts fixed by their quote).
    let mut transfers: Vec<Transfer> = if let Some(path) = value_of("--deposits") {
        load_deposit_file(&path)?
    } else {
        let target = sol_to_lamports(target_sol);
        let destinations = load_destinations()?;
        let mut out = Vec::with_capacity(destinations.len());
        for (label, to) in destinations {
            let current = rpc.get_balance(&to).await.unwrap_or(0);
            let want = jittered_lamports(target, jitter_pct)?;
            // Top up to the jittered target, never top down.
            let need = want.saturating_sub(current);
            out.push(Transfer {
                label,
                to,
                lamports: if need <= TOP_UP_DUST_LAMPORTS { 0 } else { need },
                current: Some(current),
            });
        }
        out
    };

    println!(
        "source target {:.4} SOL/wallet (±{jitter_pct}%), {} destination(s)",
        target_sol,
        transfers.len()
    );
    println!();
    println!("  {:<14} {:<44} {:>12} {:>12}", "wallet", "address", "has", "send");
    let mut total: u64 = 0;
    for t in &transfers {
        total = total.saturating_add(t.lamports);
        let has_str = match t.current {
            Some(c) => format!("{:.6}", lamports_to_sol(c)),
            None => "-".into(),
        };
        let send_str = if t.lamports == 0 {
            "skip".to_string()
        } else {
            format!("{:.6}", lamports_to_sol(t.lamports))
        };
        println!("  {:<14} {:<44} {has_str:>12} {send_str:>12}", t.label, t.to.to_string());
    }
    let funded = transfers.iter().filter(|t| t.lamports > 0).count();
    println!();
    println!(
        "total {:.6} SOL to {funded} wallet(s); {} already at target",
        lamports_to_sol(total),
        transfers.len().saturating_sub(funded)
    );

    if list_only {
        return Ok(());
    }

    let from = load_funding_keypair()?;
    let source_balance = rpc
        .get_balance(&from.pubkey())
        .await
        .map_err(|e| format!("cannot read source balance: {e}"))?;
    // Fees are per transaction: base 5,000 plus the priority fee, which at
    // COMPUTE_UNIT_LIMIT × PRIORITY_FEE_MICRO_LAMPORTS / 1e6 is tiny. Budget
    // generously — running dry half way through leaves a partly funded set.
    let fee_budget = u64::try_from(funded)
        .unwrap_or(0)
        .saturating_mul(20_000)
        .saturating_add(SOURCE_RESERVE_LAMPORTS);
    let required = total.saturating_add(fee_budget);
    println!(
        "source {} holds {:.6} SOL, needs {:.6} (transfers + fees + reserve)",
        from.pubkey(),
        lamports_to_sol(source_balance),
        lamports_to_sol(required)
    );
    if source_balance < required {
        return Err(format!(
            "source is short by {:.6} SOL — fund it or lower --target",
            lamports_to_sol(required.saturating_sub(source_balance))
        ));
    }
    if !execute {
        println!();
        println!("DRY RUN — nothing sent. Re-run with --execute to send.");
        return Ok(());
    }

    // Shuffle so the on-chain order carries no information about which wallet
    // is which, then send one at a time with a random gap.
    transfers.retain(|t| t.lamports > 0);
    shuffle(&mut transfers)?;
    // Truncate AFTER the shuffle: taking the first N of a sorted list would
    // fund buyer-01..buyer-N on the first run and walk the set in order, which
    // reintroduces exactly the index correlation the shuffle removes.
    if let Some(n) = max_wallets {
        transfers.truncate(n);
        println!("--max-wallets {n}: funding {} this run, rest remain", transfers.len());
    }

    println!();
    println!("sending {} transfer(s), one per transaction…", transfers.len());
    let mut sent = 0usize;
    let mut failed = 0usize;
    for (i, t) in transfers.iter().enumerate() {
        // A fresh blockhash per send: this run spans minutes, and one fetched
        // at the start would expire part way through.
        let blockhash = match rpc.get_latest_blockhash().await {
            Ok(h) => h,
            Err(e) => {
                println!("  {} SKIPPED — no blockhash: {e}", t.label);
                failed = failed.saturating_add(1);
                continue;
            }
        };
        match send_one(&rpc, &from, t, blockhash).await {
            Ok(sig) => {
                sent = sent.saturating_add(1);
                println!(
                    "  {:<14} {:.6} SOL  {sig}",
                    t.label,
                    lamports_to_sol(t.lamports)
                );
            }
            Err(e) => {
                failed = failed.saturating_add(1);
                println!("  {:<14} FAILED: {e}", t.label);
            }
        }
        if i.saturating_add(1) < transfers.len() {
            let delay = random_in(min_delay, max_delay)?;
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
    }
    println!();
    println!("done: {sent} sent, {failed} failed");
    if failed > 0 {
        println!("re-run to top up whatever did not land — amounts are computed from live balances,");
        println!("so a second run funds only what is still short.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_the_band() {
        let target = 150_000_000u64;
        for _ in 0..200 {
            let got = jittered_lamports(target, 12.0).unwrap();
            assert!(
                (132_000_000..=168_000_000).contains(&got),
                "{got} outside ±12% of {target}"
            );
        }
    }

    #[test]
    fn jitter_actually_varies() {
        // A "jitter" that returns the same number every time is decorative, and
        // decorative jitter is worse than none: it reads as protection.
        let target = 150_000_000u64;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            seen.insert(jittered_lamports(target, 12.0).unwrap());
        }
        assert!(seen.len() > 20, "only {} distinct amounts in 50", seen.len());
    }

    #[test]
    fn jitter_covers_both_sides_of_the_target() {
        // The bug this pins: an earlier implementation never produced a value
        // below `target` and returned `target` EXACTLY for half of all draws.
        // Fifteen of thirty wallets receiving identical amounts is the cluster
        // signature the jitter is supposed to erase, and a bounds-only test
        // passes on it happily.
        let target = 150_000_000u64;
        let (mut below, mut above, mut exact) = (0, 0, 0);
        for _ in 0..600 {
            match jittered_lamports(target, 12.0).unwrap().cmp(&target) {
                std::cmp::Ordering::Less => below += 1,
                std::cmp::Ordering::Greater => above += 1,
                std::cmp::Ordering::Equal => exact += 1,
            }
        }
        assert!(below > 200, "only {below}/600 below target — band is one-sided");
        assert!(above > 200, "only {above}/600 above target — band is one-sided");
        // With ~36M distinct lamport values in the band, repeats of the exact
        // target should be vanishingly rare.
        assert!(exact < 20, "{exact}/600 landed on the exact target");
    }

    #[test]
    fn no_amount_dominates_a_realistic_run() {
        // Thirty wallets, one run: no two should collide in practice, and a
        // mode of any size is a fingerprint.
        let mut counts: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
        for _ in 0..30 {
            *counts
                .entry(jittered_lamports(150_000_000, 12.0).unwrap())
                .or_default() += 1;
        }
        let worst = counts.values().copied().max().unwrap_or(0);
        assert!(worst <= 2, "{worst} wallets would receive an identical amount");
    }

    #[test]
    fn zero_jitter_is_exact() {
        assert_eq!(jittered_lamports(150_000_000, 0.0).unwrap(), 150_000_000);
    }

    #[test]
    fn jitter_on_zero_is_zero() {
        assert_eq!(jittered_lamports(0, 12.0).unwrap(), 0);
    }

    #[test]
    fn shuffle_permutes_without_losing_anything() {
        let mut v: Vec<u32> = (0..64).collect();
        shuffle(&mut v).unwrap();
        let mut back = v.clone();
        back.sort_unstable();
        assert_eq!(back, (0..64).collect::<Vec<_>>(), "shuffle lost or duplicated");
        assert_ne!(v, (0..64).collect::<Vec<_>>(), "shuffle did not reorder");
    }

    #[test]
    fn shuffle_handles_degenerate_sizes() {
        let mut empty: Vec<u32> = vec![];
        shuffle(&mut empty).unwrap();
        let mut one = vec![7u32];
        shuffle(&mut one).unwrap();
        assert_eq!(one, vec![7]);
    }

    #[test]
    fn random_in_respects_bounds() {
        for _ in 0..200 {
            let v = random_in(800, 4_000).unwrap();
            assert!((800..=4_000).contains(&v), "{v} out of range");
        }
        // Inverted and empty ranges must not panic — a bad delay config should
        // not take down a funding run half way through.
        assert_eq!(random_in(500, 500).unwrap(), 500);
        assert_eq!(random_in(900, 100).unwrap(), 900);
    }

    #[test]
    fn sol_lamport_round_trip() {
        assert_eq!(sol_to_lamports(0.15), 150_000_000);
        assert!((lamports_to_sol(150_000_000) - 0.15).abs() < 1e-12);
    }
}
