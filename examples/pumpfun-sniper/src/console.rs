//! Interactive console.
//!
//! The sniper fires once per mint and then stops, so buying and selling are
//! inherently two phases with an operator decision between them. This is that
//! seam: the pipeline runs in the background while a command loop holds the
//! foreground, so a snipe can be followed by `status` and `sell` in the same
//! process without losing the position context.

use {
    crate::{
        config::Config,
        dashboard,
        market::{Market, MarketTracker},
        sell::{self, SellOutcome},
        tui_log::LogRing,
    },
    futures::future::join_all,
    solana_client::{nonblocking::rpc_client::RpcClient, rpc_config::RpcTransactionConfig},
    solana_commitment_config::CommitmentConfig,
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    std::{
        collections::{HashMap, HashSet},
        io::{IsTerminal, Write},
        path::Path,
        sync::Arc,
    },
    tokio::{
        io::{AsyncBufReadExt, BufReader},
        sync::RwLock,
    },
};

/// Wallets with a sell in flight, keyed by `(wallet, mint)`.
///
/// A sell is only visible in the wallet's token balance once it confirms, which
/// takes ~10s. Two `s 50 go` inside that window both read the pre-sell balance
/// and each sell 50% of the ORIGINAL position — 75% of the coin, from an
/// operator who asked for half.
type InFlight = Arc<RwLock<HashSet<(Pubkey, Pubkey)>>>;

/// What the dispatcher wrote when it sniped. Read fresh each command so a snipe
/// landing mid-session is picked up without restarting.
#[derive(Debug)]
pub struct Position {
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub bonding_curve: Pubkey,
    pub buy_signatures: Vec<String>,
    /// Slot the launch was seen in. Only used to order positions; absent on
    /// records written before it was recorded.
    pub create_slot: Option<u64>,
}

pub fn load_positions() -> Vec<Position> {
    let dir = Path::new("positions");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut positions: Vec<Position> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let body = std::fs::read_to_string(e.path()).ok()?;
            let v: serde_json::Value = serde_json::from_str(&body).ok()?;
            Some(Position {
                mint: v.get("mint")?.as_str()?.parse().ok()?,
                creator: v.get("creator")?.as_str()?.parse().ok()?,
                bonding_curve: v.get("bonding_curve")?.as_str()?.parse().ok()?,
                buy_signatures: v
                    .get("buy_signatures")
                    .and_then(|s| s.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                create_slot: v.get("create_slot").and_then(serde_json::Value::as_u64),
            })
        })
        .collect();
    sort_positions(&mut positions);
    positions
}

/// Newest first, ties broken by mint.
///
/// `read_dir` returns entries in whatever order the filesystem hands back, which
/// is neither sorted nor stable between calls. The panel and the sell path both
/// resolved "the" position through `first()`, so an unordered list could show
/// mint A while `s 100 go` sold mint B.
fn sort_positions(positions: &mut [Position]) {
    positions.sort_by(|a, b| {
        b.create_slot
            .cmp(&a.create_slot)
            .then_with(|| a.mint.to_string().cmp(&b.mint.to_string()))
    });
}

/// Move the command target to the front.
///
/// `dashboard::refresh` derives the panel's mint and cost basis from
/// `positions.first()`, so this is what keeps the panel showing the position a
/// sell would actually hit.
fn order_for_target(mut positions: Vec<Position>, selected: Option<Pubkey>) -> Vec<Position> {
    if let Some(mint) = selected {
        if let Some(i) = positions.iter().position(|p| p.mint == mint) {
            let target = positions.remove(i);
            positions.insert(0, target);
        }
    }
    positions
}

/// The position a command acts on, and how it was arrived at.
struct Target<'a> {
    position: &'a Position,
    /// Set when the target was defaulted rather than named by the operator and
    /// other positions are open. Callers on the money path log it verbatim; it
    /// is a note, never an error, so it can never stop a sell.
    note: Option<String>,
}

/// Resolve which position a command acts on: the newest, unless the operator
/// named one.
///
/// This used to refuse whenever more than one position was on disk. The bug it
/// was guarding against was not "chose without asking" — it was choosing
/// SILENTLY and arbitrarily, out of an unordered `read_dir`, so the panel could
/// show mint A while `s 100 go` sold mint B. Ordering fixed the arbitrary half
/// (`sort_positions`), and `note` plus the mint in the sell line fix the silent
/// half. Refusing on top of both only stands between an operator and the exit
/// they just asked for, with stale files from old snipes as the blocker.
///
/// Positions every wallet has already exited are skipped when defaulting: they
/// are exactly the files that accumulate, and they are the ones an operator
/// never means. `exited` is the set published by the background scan, so this
/// stays a pure in-memory decision — no RPC on the sell path.
fn select_position<'a>(
    positions: &'a [Position],
    selected: Option<Pubkey>,
    exited: &HashSet<Pubkey>,
) -> Result<Target<'a>, String> {
    if let Some(mint) = selected {
        // An explicit choice is honoured even if it looks exited — the operator
        // named it, and falling back to "some other position" here would sell a
        // coin they did not choose.
        return positions
            .iter()
            .find(|p| p.mint == mint)
            .map(|position| Target {
                position,
                note: None,
            })
            .ok_or_else(|| format!("selected position {mint} is no longer on disk"));
    }
    // `positions` is sorted newest-first, so the first position still holding
    // tokens is the newest one. If every position looks exited, take the newest
    // anyway: this is a preference, not a filter, and it must never leave a
    // command with no target.
    let Some(position) = positions
        .iter()
        .find(|p| !exited.contains(&p.mint))
        .or_else(|| positions.first())
    else {
        return Err("no position — nothing sniped in this working directory".into());
    };
    let others = positions.len().saturating_sub(1);
    let note = (others > 0).then(|| {
        let stale = positions
            .iter()
            .filter(|p| p.mint != position.mint && exited.contains(&p.mint))
            .count();
        let stale = if stale > 0 {
            format!(", {stale} already exited")
        } else {
            String::new()
        };
        format!(
            "target {} (newest) — {others} other position(s) open{stale}. \
             'select <mint>' to target another, 'positions' lists them.",
            position.mint
        )
    });
    Ok(Target { position, note })
}

/// Positions whose every buyer wallet was observed holding zero tokens.
///
/// Selling must never wait on RPC, so this runs on the same background timer as
/// the panel and publishes a set the command loop only reads from memory. The
/// work is bounded on purpose: the newest few positions only — the default can
/// never land past them — batched into `getMultipleAccounts` requests.
///
/// Anything that could not be read counts as still holding. Preferring a
/// position that is in fact empty costs one rejected sell; skipping one that
/// still holds tokens would silently retarget the sell, which is the failure
/// mode this whole path exists to avoid.
async fn scan_exited(cfg: &Config, rpc: &RpcClient, positions: &[Position]) -> HashSet<Pubkey> {
    const MAX_POSITIONS_SCANNED: usize = 8;
    /// `getMultipleAccounts` is capped at 100 keys per request.
    const KEYS_PER_REQUEST: usize = 100;
    /// SPL token layout: `amount` is a little-endian u64 at offset 64. Token-2022
    /// keeps that base account intact and appends its extensions after it.
    const AMOUNT: std::ops::Range<usize> = 64..72;

    // With no wallets there is nothing that could hold anything, and every
    // position would come back "exited". Say nothing instead.
    if cfg.buyers.is_empty() {
        return HashSet::new();
    }
    let scanned: Vec<Pubkey> = positions
        .iter()
        .take(MAX_POSITIONS_SCANNED)
        .map(|p| p.mint)
        .collect();
    let mut keys: Vec<Pubkey> = Vec::new();
    let mut mints: Vec<Pubkey> = Vec::new();
    for mint in &scanned {
        for buyer in &cfg.buyers {
            keys.push(crate::pump::pdas::associated_token_address_with_program(
                &buyer.keypair.pubkey(),
                mint,
                &crate::pump::pdas::TOKEN_2022_PROGRAM_ID,
            ));
            mints.push(*mint);
        }
    }

    let mut holding: HashSet<Pubkey> = HashSet::new();
    for (batch, chunk) in keys.chunks(KEYS_PER_REQUEST).enumerate() {
        let base = batch.saturating_mul(KEYS_PER_REQUEST);
        let fetched = rpc.get_multiple_accounts(chunk).await.ok();
        for i in 0..chunk.len() {
            let Some(mint) = mints.get(base.saturating_add(i)).copied() else {
                continue;
            };
            let empty = match fetched.as_ref().map(|accounts| accounts.get(i)) {
                // The request failed, or the response was short: nothing was
                // learned about this wallet, so assume it still holds.
                None | Some(None) => false,
                // No account is a real zero — the ATA was never opened, or it
                // was closed on the way out of the position.
                Some(Some(None)) => true,
                Some(Some(Some(account))) => {
                    account.owner == crate::pump::pdas::TOKEN_2022_PROGRAM_ID
                        && account
                            .data
                            .get(AMOUNT)
                            .and_then(|raw| <[u8; 8]>::try_from(raw).ok())
                            .map(u64::from_le_bytes)
                            == Some(0)
                }
            };
            if !empty {
                holding.insert(mint);
            }
        }
    }
    scanned
        .into_iter()
        .filter(|mint| !holding.contains(mint))
        .collect()
}

/// Cost basis derived from the buy transactions, plus how much of it we could
/// actually read.
///
/// The two are inseparable: a partial basis understates cost, which OVERSTATES
/// P&L, which is the direction that talks an operator out of an exit. Callers
/// must decide what to do with an incomplete figure rather than receive a bare
/// number that looks authoritative.
pub struct CostBasis {
    pub lamports: i128,
    /// Buy transactions accounted for, out of how many the position recorded.
    pub counted: usize,
    pub recorded: usize,
}

impl CostBasis {
    pub fn is_complete(&self) -> bool {
        self.counted == self.recorded
    }

    pub fn sol(&self) -> f64 {
        self.lamports as f64 / 1e9
    }
}

/// Cost basis straight from the buy transactions — the fee payer's SOL delta,
/// so priority fees, tips and rent are already included rather than modelled.
pub async fn cost_basis(rpc: &RpcClient, position: &Position) -> Option<CostBasis> {
    let mut total: i128 = 0;
    let mut counted: usize = 0;
    for sig in &position.buy_signatures {
        let Ok(parsed) = sig.parse() else { continue };
        let Ok(tx) = rpc
            .get_transaction_with_config(
                &parsed,
                RpcTransactionConfig {
                    max_supported_transaction_version: Some(0),
                    // The default commitment is `finalized`, ~30s behind the
                    // tip. That is exactly the window in which the decision to
                    // sell is made, so the buys that just landed returned Err
                    // and dropped silently out of the basis.
                    commitment: Some(CommitmentConfig::confirmed()),
                    ..Default::default()
                },
            )
            .await
        else {
            continue;
        };
        // One unreadable transaction must not discard the ones already
        // accumulated: `?` here returned None for the whole position, turning a
        // single missing buy into "cost unavailable".
        let Some(meta) = tx.transaction.meta else {
            continue;
        };
        let (Some(pre), Some(post)) = (meta.pre_balances.first(), meta.post_balances.first()) else {
            continue;
        };
        total = total.saturating_add(i128::from(*pre).saturating_sub(i128::from(*post)));
        counted = counted.saturating_add(1);
    }
    (counted > 0).then_some(CostBasis {
        lamports: total,
        counted,
        recorded: position.buy_signatures.len(),
    })
}

/// Cost basis for callers that can only render a bare number.
///
/// The panel has nowhere to put a qualifier, so an incomplete basis is reported
/// as unavailable rather than as a figure the operator would act on — the panel
/// already has a rendering for unknown cost, and no P&L beats wrong P&L.
pub async fn cost_basis_lamports(rpc: &RpcClient, position: &Position) -> Option<i128> {
    cost_basis(rpc, position)
        .await
        .filter(CostBasis::is_complete)
        .map(|c| c.lamports)
}

/// Which wallets a command acts on.
///
/// `All` is kept distinct from an explicit list of every index rather than
/// expanded at parse time: the parser does not know how many buyers are
/// configured, and resolving `All` against a stale count would silently skip
/// a wallet added since. Expansion happens at the call site, against
/// `cfg.buyers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletSel {
    All,
    Only(Vec<usize>),
}

impl WalletSel {
    /// Concrete buyer indices, in ascending order, refusing any index the
    /// configuration cannot satisfy.
    ///
    /// Out-of-range is an error for the whole command rather than a filter.
    /// `s 1,2,9 100 go` from an operator who believes there are ten wallets
    /// must not quietly sell two of them and report success.
    pub fn resolve(&self, total: usize) -> Result<Vec<usize>, String> {
        match self {
            Self::All => Ok((0..total).collect()),
            Self::Only(indices) => {
                if let Some(bad) = indices.iter().find(|n| **n >= total) {
                    return Err(format!(
                        "wallet {bad} out of range — {total} wallet(s) configured (0..{})",
                        total.saturating_sub(1)
                    ));
                }
                Ok(indices.clone())
            }
        }
    }
}

/// A parsed `sell` command.
///
/// Kept pure and separate so the grammar can be tested: this is the money path,
/// and its previous form parsed the wallet index as `f64` and cast it with `as
/// usize`. Float→int casts SATURATE rather than fail, so `s -1 50 go` and
/// `s nan 50 go` both resolved to wallet 0 and sold a wallet nobody named.
#[derive(Debug, PartialEq)]
pub struct SellCommand {
    pub wallets: WalletSel,
    pub pct: f64,
    pub execute: bool,
}

/// A parsed `buy` command. Same shape as a sell, and deliberately so — one
/// grammar, so `1,2` cannot mean different wallets on the two sides.
///
/// `pct` is a percentage of each wallet's *SOL balance*, not of a position.
#[derive(Debug, PartialEq)]
pub struct BuyCommand {
    pub wallets: WalletSel,
    pub pct: f64,
    pub execute: bool,
}

/// Grammar: `s <pct> [go]` | `s <wallets> <pct> [go]`.
///
/// `<wallets>` is `all`, a single index, a comma list (`1,2,3`) or a range
/// (`1-3`). `parts` is the whole command line, including the command word.
pub fn parse_sell(parts: &[&str]) -> Result<SellCommand, String> {
    const USAGE: &str = "usage: s <pct> [go] | s <wallets> <pct> [go]   \
                         (wallets: 1 | 1,2 | 1-3 | all)";
    let (wallets, pct, execute) = parse_wallets_pct(parts, USAGE)?;
    Ok(SellCommand {
        wallets,
        pct,
        execute,
    })
}

/// Grammar: `b <pct> [go]` | `b <wallets> <pct> [go]`, matching `s` exactly.
pub fn parse_buy(parts: &[&str]) -> Result<BuyCommand, String> {
    const USAGE: &str = "usage: b <pct> [go] | b <wallets> <pct> [go]   \
                         (pct is % of each wallet's SOL balance)";
    let (wallets, pct, execute) = parse_wallets_pct(parts, USAGE)?;
    Ok(BuyCommand {
        wallets,
        pct,
        execute,
    })
}

/// The shared body of both grammars. One implementation so `b` and `s` can
/// never drift into meaning different things by the same words.
fn parse_wallets_pct(parts: &[&str], usage: &str) -> Result<(WalletSel, f64, bool), String> {
    let mut args: Vec<&str> = parts.iter().skip(1).copied().collect();
    let execute = matches!(args.last(), Some(&"go"));
    if execute {
        args.pop();
    }
    match args.as_slice() {
        [pct] => Ok((WalletSel::All, parse_pct(pct)?, execute)),
        [wallets, pct] => Ok((parse_wallet_set(wallets)?, parse_pct(pct)?, execute)),
        _ => Err(usage.into()),
    }
}

/// `all` | `2` | `0,1,3` | `1-3`, in any mix (`0,2-4`).
///
/// Duplicates are collapsed and the result is sorted, so `s 2,1,2 50 go` sells
/// wallets 1 and 2 once each. Sending a wallet's sell twice in one batch would
/// read the same pre-sell balance for both and sell the percentage twice.
fn parse_wallet_set(raw: &str) -> Result<WalletSel, String> {
    if raw.eq_ignore_ascii_case("all") || raw == "*" {
        return Ok(WalletSel::All);
    }
    let mut out: Vec<usize> = Vec::new();
    for piece in raw.split(',').filter(|p| !p.is_empty()) {
        match piece.split_once('-') {
            Some((lo, hi)) => {
                let (lo, hi) = (parse_wallet_index(lo)?, parse_wallet_index(hi)?);
                if lo > hi {
                    return Err(format!("range '{piece}' runs backwards — write {hi}-{lo}"));
                }
                out.extend(lo..=hi);
            }
            None => out.push(parse_wallet_index(piece)?),
        }
    }
    if out.is_empty() {
        return Err(format!("no wallets in '{raw}' — try 1, 1,2, 1-3 or all"));
    }
    out.sort_unstable();
    out.dedup();
    Ok(WalletSel::Only(out))
}

/// A wallet index is an index, so it is parsed as an integer and anything else
/// is refused. Nothing here may fall back to a lenient numeric parse.
fn parse_wallet_index(raw: &str) -> Result<usize, String> {
    raw.trim().parse::<usize>().map_err(|_| {
        format!("wallet must be a whole number (0, 1, 2, …), got '{raw}' — refusing to guess")
    })
}

fn parse_pct(raw: &str) -> Result<f64, String> {
    let pct: f64 = raw
        .parse()
        .map_err(|_| format!("pct must be a number, got '{raw}'"))?;
    // "nan" and "inf" parse cleanly as f64, and every comparison against them is
    // false — including a range check, which then reads as "out of range" for
    // one and would read as "in range" for any inverted test. Name them.
    if !pct.is_finite() {
        return Err(format!("pct must be a finite number, got '{raw}'"));
    }
    if !(0.0..=100.0).contains(&pct) {
        return Err(format!("pct must be between 0 and 100, got {pct}"));
    }
    Ok(pct)
}

/// Restores the terminal on every exit path, including a panic.
///
/// Raw mode plus the alternate screen are process-global state. Restoring them
/// only where the loop returns normally means any panic leaves the operator
/// staring at a shell with no echo, no line editing and a live position open.
struct TerminalGuard;

impl TerminalGuard {
    /// Enter the TUI, installing a panic hook that restores first so the panic
    /// message lands on the normal screen instead of inside the one we took.
    fn enter() -> Option<Self> {
        use crossterm::{execute, terminal::EnterAlternateScreen};
        if crossterm::terminal::enable_raw_mode().is_err() {
            return None;
        }
        if execute!(std::io::stdout(), EnterAlternateScreen).is_err() {
            let _ = crossterm::terminal::disable_raw_mode();
            return None;
        }
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            Self::restore();
            previous(info);
        }));
        Some(Self)
    }

    fn restore() {
        use crossterm::{execute, terminal::LeaveAlternateScreen};
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        Self::restore();
    }
}

/// Report each outcome into the events panel. Realised proceeds are not
/// included — they arrive from `report_proceeds` once the sells confirm.
fn summarise(outcomes: &[SellOutcome]) {
    for o in outcomes {
        match (&o.error, &o.signature) {
            (Some(err), _) => log::warn!("#{} {}: {err}", o.index, o.wallet),
            (None, Some(sig)) => log::info!(
                "#{} SOLD {} tokens  sig {}",
                o.index,
                o.tokens,
                sig.get(..16).unwrap_or(sig)
            ),
            (None, None) => log::info!(
                "#{} SIMULATE OK — would sell {} tokens",
                o.index,
                o.tokens
            ),
        }
    }
}

/// Run the command loop until EOF or `quit`.
///
/// Two cadences: the panel redraws every second from in-memory market state,
/// while the RPC-backed half (balances, cost basis, SOL/USD) refreshes on a
/// slower timer. Polling wallets at 1 Hz would rate-limit the same endpoint the
/// dispatch hot path depends on.
/// Everything the `b` command needs to put a buy on the wire.
///
/// Grouped into one struct rather than four more parameters on `run`, which
/// already carries six.
pub struct Buying {
    pub pool: Arc<crate::sender::rpc::RpcPool>,
    pub fast: Arc<crate::sender::fast::FastSenderPool>,
    pub statics: Arc<crate::pump::instructions::StaticAccounts>,
    pub fills: crate::fills::FillLog,
    /// Opening v2 curve state, carried only for its fee basis points. The
    /// reserves are replaced with live ones read off the coin's bonding curve
    /// — a manual buy happens long after launch, when the opening reserves
    /// describe nothing. There is no v1 counterpart because a manual buy
    /// refuses v1 coins: the position record does not store
    /// `associated_bonding_curve`, which a v1 buy needs.
    pub curve_v2: crate::pump::quote::CurveState,
}

pub async fn run(
    cfg: Arc<Config>,
    rpc: Arc<RpcClient>,
    watched: Arc<RwLock<HashSet<Pubkey>>>,
    market: Arc<RwLock<MarketTracker>>,
    logs: LogRing,
    // The same warm blockhash the dispatcher uses, refreshed every 400ms by a
    // background task. Selling reads it rather than calling
    // `get_latest_blockhash`: that round trip is what the zero-RPC hot-path
    // rule exists to avoid, and it is redundant when a fresh hash is already
    // in memory.
    blockhash: Arc<RwLock<solana_hash::Hash>>,
    buying: Buying,
) {
    if let Err(err) = sell::verify_layout() {
        log::error!("sell layout verification FAILED — selling is disabled:\n{err}");
    }

    // Which position commands act on. Shared with the refresh task so the panel
    // and the sell path can never disagree about the target mint.
    let selected: Arc<RwLock<Option<Pubkey>>> = Arc::new(RwLock::new(None));
    // Mints the background scan last saw at zero across every wallet. Read-only
    // on the command path, so defaulting away from a stale position costs no
    // round trip when `s 100 go` is typed.
    let exited: Arc<RwLock<HashSet<Pubkey>>> = Arc::new(RwLock::new(HashSet::new()));
    let in_flight: InFlight = Arc::new(RwLock::new(HashSet::new()));

    // Register the tracker before the refresh task starts. `dashboard` also
    // registers it lazily from the render path, but that only fires once the
    // TUI has drawn a frame — so headless/scripted runs would never poll the
    // bonding curve, and the first few seconds of an interactive run would
    // silently skip it. Doing it here makes the poll independent of render
    // order.
    dashboard::attach_tracker(&market);

    let snapshot = Arc::new(RwLock::new(dashboard::Snapshot {
        live: !cfg.dry_run,
        feed: format!("{:?}", cfg.datasource).to_lowercase(),
        routes: format!("{:?}", cfg.send_paths),
        // Total across every wallet, not wallet #0's share. In BUY_SIZING=
        // balance the thirty sizes differ by design, so a single sample is
        // wrong for twenty-nine of them and hides total exposure — which is the
        // number an operator actually needs while a launch is running.
        buy_sol: cfg
            .buyers
            .iter()
            .map(|b| b.buy_amount_lamports())
            .fold(0u64, u64::saturating_add) as f64
            / 1e9,
        ..Default::default()
    }));
    {
        let (cfg, rpc, snap, selected, exited) = (
            Arc::clone(&cfg),
            Arc::clone(&rpc),
            Arc::clone(&snapshot),
            Arc::clone(&selected),
            Arc::clone(&exited),
        );
        tokio::spawn(async move {
            loop {
                let positions = load_positions();
                // Refresh the exited set first, then resolve through the same
                // function the sell path uses. The panel showing one mint while
                // a sell hits another is the bug this file keeps guarding
                // against, so there is exactly one resolver.
                let stale = scan_exited(&cfg, &rpc, &positions).await;
                *exited.write().await = stale.clone();
                let target = *selected.read().await;
                let resolved = select_position(&positions, target, &stale)
                    .ok()
                    .map(|t| t.position.mint);
                let positions = order_for_target(positions, resolved);
                dashboard::refresh(&cfg, &rpc, &positions, &snap).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }

    // `enable_raw_mode()` is not a stdin test: on Unix crossterm acts on the
    // controlling terminal, so it succeeds with stdin piped and the TUI branch
    // then blocks forever on key events that will never arrive. Ask stdin (and
    // stdout, which ratatui draws to) directly instead.
    let scripted = !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal());
    let mut guard = if scripted { None } else { TerminalGuard::enter() };
    let mut terminal = guard.as_ref().and_then(|_| {
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout())).ok()
    });
    if guard.is_some() && terminal.is_none() {
        // The screen was already taken when the backend failed. Give it back
        // rather than driving a plain prompt under raw mode.
        guard = None;
        log::warn!("could not start the panel — falling back to a plain prompt");
    }
    if terminal.is_none() {
        // No alternate screen here, so plain stdout is safe — and it is the only
        // output the operator sees, since interactive mode routes `log::` into
        // the ring and `sniper.log` rather than the terminal.
        println!("sniper console (no panel) — 'help' for commands, output in sniper.log");
    }
    log::info!("interactive console — 'help' for commands. Panel refreshes live.");

    let mut input = String::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        let line = if let Some(term) = terminal.as_mut() {
            match read_line_with_panel(&mut input, &snapshot, &market, &watched, &logs, term).await {
                Some(l) => l,
                None => break,
            }
        } else {
            print!("sniper> ");
            let _ = std::io::stdout().flush();
            match lines.next_line().await {
                Ok(Some(l)) => l,
                _ => break,
            }
        };
        let parts: Vec<&str> = line.split_whitespace().collect();
        let positions = load_positions();
        let target = *selected.read().await;
        // One in-memory snapshot per command: the sell path must never resolve
        // its target through a lock the refresh task can be holding across RPC.
        let stale = exited.read().await.clone();
        match parts.first().copied() {
            None => continue,
            Some("help") => print_help(),
            Some("quit") | Some("exit") => break,
            Some("price") => match sell::sol_usd(&rpc).await {
                Some(p) => log::info!("SOL/USD ${p:.2} (on-chain Pyth pull oracle)"),
                None => log::info!("no fresh on-chain price available"),
            },
            Some("watch") => match parts.get(1).and_then(|p| p.parse::<Pubkey>().ok()) {
                Some(pk) => {
                    watched.write().await.insert(pk);
                    log::info!("watching {pk} — launches from this creator will be sniped");
                }
                None => log::info!("usage: watch <creator_pubkey>"),
            },
            Some("unwatch") => match parts.get(1).and_then(|p| p.parse::<Pubkey>().ok()) {
                Some(pk) => {
                    let removed = watched.write().await.remove(&pk);
                    log::info!("{}", if removed { "unwatched" } else { "was not watched" });
                }
                None => log::info!("usage: unwatch <creator_pubkey>"),
            },
            Some("watching") => {
                let w = watched.read().await;
                if w.is_empty() {
                    log::info!("watching nothing — use 'watch <creator>'");
                }
                for pk in w.iter() {
                    log::info!("  {pk}");
                }
            }
            Some("market") => cmd_market(&market).await,
            Some("wallets") => {
                let mint = select_position(&positions, target, &stale)
                    .ok()
                    .map(|t| t.position.mint);
                cmd_wallets(&cfg, &rpc, mint).await
            }
            Some("positions") => {
                if positions.is_empty() {
                    log::info!("none");
                }
                let chosen = select_position(&positions, target, &stale).ok();
                let resolved = chosen.as_ref().map(|t| t.position.mint);
                for p in &positions {
                    log::info!(
                        "  {} {} {} creator {}",
                        if Some(p.mint) == resolved { "->" } else { "  " },
                        p.mint,
                        // Same width either way, so the creator column lines up.
                        if stale.contains(&p.mint) { "(exited)" } else { "        " },
                        p.creator
                    );
                }
                // Say why that one carries the arrow. The listing is where an
                // operator goes to check the target, so the defaulted choice
                // has to explain itself here too.
                if let Some(note) = chosen.and_then(|t| t.note) {
                    log::info!("{note}");
                }
            }
            Some("select") | Some("sel") => match parts.get(1).copied() {
                Some("auto") | Some("none") => {
                    *selected.write().await = None;
                    log::info!("target cleared — 's' follows the newest open position again");
                }
                Some(raw) => match raw.parse::<Pubkey>() {
                    Ok(mint) if positions.iter().any(|p| p.mint == mint) => {
                        *selected.write().await = Some(mint);
                        log::info!("target {mint} — 's' sells this position");
                    }
                    Ok(mint) => log::warn!("no position on disk for {mint} — 'positions' lists them"),
                    Err(_) => log::info!("usage: select <mint> | select auto"),
                },
                None => log::info!("usage: select <mint> | select auto"),
            },
            Some("status") => {
                let usd = sell::sol_usd(&rpc).await;
                let resolved = select_position(&positions, target, &stale)
                    .ok()
                    .map(|t| t.position.mint);
                cmd_status(&cfg, &rpc, &market, &positions, resolved, usd).await;
            }
            Some("buy") | Some("b") => {
                let cmd = match parse_buy(&parts) {
                    Ok(cmd) => cmd,
                    Err(err) => {
                        log::warn!("{err}");
                        continue;
                    }
                };
                // Same resolver as the sell path, deliberately: `b` and `s`
                // must never disagree about which coin they are acting on.
                let position = match select_position(&positions, target, &stale) {
                    Ok(chosen) => {
                        if let Some(note) = &chosen.note {
                            log::info!("{note}");
                        }
                        chosen.position
                    }
                    Err(err) => {
                        log::warn!("{err}");
                        continue;
                    }
                };
                let wallets = match cmd.wallets.resolve(cfg.buyers.len()) {
                    Ok(wallets) => wallets,
                    Err(err) => {
                        log::warn!("{err}");
                        continue;
                    }
                };
                cmd_buy(
                    &cfg,
                    &rpc,
                    &buying,
                    position,
                    &wallets,
                    cmd.pct,
                    cmd.execute,
                    *blockhash.read().await,
                )
                .await;
            }
            Some("fills") => cmd_fills(&cfg, &buying.fills, &positions, target, &stale).await,
            Some("sell") | Some("s") => {
                let cmd = match parse_sell(&parts) {
                    Ok(cmd) => cmd,
                    Err(err) => {
                        log::warn!("{err}");
                        continue;
                    }
                };
                let position = match select_position(&positions, target, &stale) {
                    Ok(chosen) => {
                        // Before anything is sent, and before any other
                        // warning can scroll it away.
                        if let Some(note) = &chosen.note {
                            log::info!("{note}");
                        }
                        chosen.position
                    }
                    Err(err) => {
                        log::warn!("{err}");
                        continue;
                    }
                };
                if sell::verify_layout().is_err() {
                    log::warn!("refusing to sell: layout verification failed");
                    continue;
                }
                // fee_recipient and the buyback recipient come from the live
                // Global account; a stale value fails every sell.
                let Ok(global_acct) = rpc.get_account(&crate::pump::pdas::global()).await else {
                    log::warn!("could not fetch Global");
                    continue;
                };
                let Some(global) =
                    carbon_pumpfun_decoder::accounts::global::Global::decode(&global_acct.data)
                else {
                    log::warn!("could not decode Global");
                    continue;
                };
                let buyback = global
                    .buyback_fee_recipients
                    .first()
                    .copied()
                    .unwrap_or(global.fee_recipient);

                let mut targets: Vec<usize> = match cmd.wallets.resolve(cfg.buyers.len()) {
                    Ok(targets) => targets,
                    Err(err) => {
                        log::warn!("{err}");
                        continue;
                    }
                };
                // Claim only for real sends: a simulation changes no balance and
                // must not lock a wallet out of the sell it is rehearsing.
                if cmd.execute {
                    let mut guard = in_flight.write().await;
                    let (ready, busy): (Vec<usize>, Vec<usize>) = targets
                        .into_iter()
                        .partition(|n| guard.insert((cfg.buyers[*n].keypair.pubkey(), position.mint)));
                    for n in busy {
                        log::warn!(
                            "#{n} skipped — a sell is still confirming, so its balance would be \
                             read pre-sell and this would sell {}% of the ORIGINAL position again",
                            cmd.pct
                        );
                    }
                    targets = ready;
                }
                if targets.is_empty() {
                    log::warn!("nothing to sell");
                    continue;
                }
                let pct = cmd.pct;
                log::info!(
                    "selling {pct}% of {} from {} wallet(s) — {}",
                    position.mint,
                    targets.len(),
                    if cmd.execute { "EXECUTING" } else { "simulate only" }
                );
                // Read the warm blockhash rather than fetching one. It is at
                // most 400ms old against a ~60s validity window.
                let blockhash = *blockhash.read().await;
                // Concurrent — sequentially these took ~10s each and froze the
                // panel for the whole batch.
                let outcomes: Vec<_> = join_all(targets.into_iter().map(|n| {
                    let rpc = Arc::clone(&rpc);
                    let cfg = Arc::clone(&cfg);
                    let (mint, curve, creator) =
                        (position.mint, position.bonding_curve, position.creator);
                    let (fee_recipient, buyback) = (global.fee_recipient, buyback);
                    async move {
                        sell::sell_one(
                            &rpc,
                            &cfg.buyers[n].keypair,
                            n,
                            &mint,
                            &curve,
                            &creator,
                            &fee_recipient,
                            &buyback,
                            pct,
                            cmd.execute,
                            blockhash,
                        )
                        .await
                    }
                }))
                .await;
                // Realised proceeds need ~10s of confirmation; measured detached
                // so the panel keeps updating and the figure lands in `events`.
                // The same wait is what clears the in-flight claim, so the guard
                // covers exactly the window in which balances are stale.
                for o in &outcomes {
                    match &o.signature {
                        Some(sig) => {
                            let rpc = Arc::clone(&rpc);
                            let in_flight = Arc::clone(&in_flight);
                            let (w, i, before, sig) =
                                (o.wallet, o.index, o.balance_before, sig.clone());
                            let mint = position.mint;
                            let cfg_refresh = Arc::clone(&cfg);
                            tokio::spawn(async move {
                                sell::report_proceeds(Arc::clone(&rpc), w, i, before, sig).await;
                                in_flight.write().await.remove(&(w, mint));
                                // A sell puts SOL back, so the wallet can buy
                                // again. `report_proceeds` has already waited
                                // for confirmation, so the balance it reads is
                                // settled. Without this the wallet stays sized
                                // at whatever it held before the sell — which
                                // after a snipe is ~0 — and it sits out the
                                // next launch despite being funded.
                                crate::wallets::refresh_buy_sizes(&cfg_refresh, &rpc).await;
                            });
                        }
                        // Nothing was sent, so nothing is confirming. Only
                        // release a claim this command made.
                        None if cmd.execute => {
                            in_flight.write().await.remove(&(o.wallet, position.mint));
                        }
                        None => {}
                    }
                }
                summarise(&outcomes);
            }
            Some(other) => log::info!("unknown command '{other}' — try 'help'"),
        }
    }
    drop(terminal);
    drop(guard);
    println!("bye");
}

/// Execute `b` for one coin across chosen wallets.
///
/// The version is decided from the *mint account's owner program* rather than
/// from anything on the position record: v2 mints are Token-2022 and v1 mints
/// are classic SPL Token, and that is a fact of the chain rather than of a
/// file that may have been written by an older build. Position records carry
/// no version field, so guessing from the record would silently build a v1
/// instruction for a v2 coin — 26 accounts where 27 are needed.
#[allow(clippy::too_many_arguments)]
async fn cmd_buy(
    cfg: &Arc<Config>,
    rpc: &Arc<RpcClient>,
    buying: &Buying,
    position: &Position,
    wallets: &[usize],
    pct: f64,
    execute: bool,
    blockhash: solana_hash::Hash,
) {
    use crate::pump::{instructions::CoinAccountsV2, pdas};

    let Ok(mint_acct) = rpc.get_account(&position.mint).await else {
        log::warn!("could not read mint {} — not buying", position.mint);
        return;
    };
    if mint_acct.owner != pdas::TOKEN_2022_PROGRAM_ID {
        // v1 needs `associated_bonding_curve`, which the position record does
        // not store. Refusing is the honest outcome: a buy built on a derived
        // guess is a buy that fails after paying fees.
        log::warn!(
            "{} is a v1 (SPL Token) coin — manual buy currently supports create_v2 coins only",
            position.mint
        );
        return;
    }

    // Live reserves. `min_tokens_out` is 1 for v2 unless V2_TRUST_QUOTE is set,
    // so these only matter for the quote shown to the operator — but showing a
    // quote off opening reserves would be worse than showing none.
    let mut curve = buying.curve_v2;
    match rpc.get_account(&position.bonding_curve).await {
        Ok(acct) => {
            match carbon_pumpfun_decoder::accounts::bonding_curve::BondingCurve::decode(&acct.data)
            {
                Some(bc) if bc.virtual_token_reserves > 0 && bc.virtual_quote_reserves > 0 => {
                    curve.virtual_sol_reserves = bc.virtual_quote_reserves;
                    curve.virtual_token_reserves = bc.virtual_token_reserves;
                }
                _ => log::warn!("could not decode the bonding curve — quoting off opening state"),
            }
        }
        Err(err) => log::warn!("could not read the bonding curve ({err}) — quoting off opening state"),
    }

    let coin = crate::dispatch::Coin::V2(CoinAccountsV2::new(
        position.mint,
        position.bonding_curve,
        &position.creator,
        // Mayhem only changes which program the OPTIONAL trailing
        // `bonding_curve_v2` account derives on, and `new` leaves that account
        // off. Passing false is therefore inert here, not a guess.
        false,
    ));

    // Balances concurrently: with four wallets this is one round trip's
    // latency rather than four.
    let balances = join_all(wallets.iter().map(|n| {
        let rpc = Arc::clone(rpc);
        let pk = cfg.buyers.get(*n).map(|b| b.keypair.pubkey());
        async move {
            match pk {
                Some(pk) => rpc.get_balance(&pk).await.ok(),
                None => None,
            }
        }
    }))
    .await;

    let mut sizes: Vec<crate::dispatch::BuySize> = Vec::new();
    for (n, balance) in wallets.iter().zip(balances) {
        let Some(buyer) = cfg.buyers.get(*n) else {
            continue;
        };
        let Some(balance) = balance else {
            log::warn!("#{n} skipped — could not read its SOL balance");
            continue;
        };
        let reserve = crate::dispatch::gas_reserve(cfg, buyer);
        let spend = crate::dispatch::size_buy(balance, reserve, pct);
        if spend == 0 {
            log::warn!(
                "#{n} skipped — {:.6} SOL balance does not cover the {:.6} SOL fee reserve",
                lamports_to_sol(balance),
                lamports_to_sol(reserve)
            );
            continue;
        }
        log::info!(
            "#{n} buy {:.6} SOL (balance {:.6}, reserve {:.6})",
            lamports_to_sol(spend),
            lamports_to_sol(balance),
            lamports_to_sol(reserve)
        );
        sizes.push(crate::dispatch::BuySize {
            buyer: *n,
            balance,
            reserve,
            spend,
        });
    }
    if sizes.is_empty() {
        log::warn!("nothing to buy");
        return;
    }
    let total: u64 = sizes.iter().map(|s| s.spend).fold(0, u64::saturating_add);
    log::info!(
        "buying {} with {:.6} SOL across {} wallet(s) — {}",
        position.mint,
        lamports_to_sol(total),
        sizes.len(),
        if execute { "EXECUTING" } else { "simulate only" }
    );

    let results = crate::dispatch::manual_buy(
        cfg,
        &buying.statics,
        &buying.pool,
        &buying.fast,
        &coin,
        &curve,
        &sizes,
        blockhash,
        execute,
    )
    .await;
    for (n, result) in results {
        match result {
            Ok(sig) if execute => log::info!("#{n} SENT {sig}"),
            Ok(_) => log::info!("#{n} built (simulate only — add 'go' to send)"),
            Err(err) => log::error!("#{n} BUY FAILED: {err}"),
        }
    }
}

fn lamports_to_sol(lamports: u64) -> f64 {
    // Lamport counts are far inside f64's exact-integer range.
    #[allow(clippy::cast_precision_loss)]
    let sol = lamports as f64 / 1e9;
    sol
}

/// `fills` — per-wallet buy outcome for the coin commands would act on.
async fn cmd_fills(
    cfg: &Arc<Config>,
    fills: &crate::fills::FillLog,
    positions: &[Position],
    selected: Option<Pubkey>,
    exited: &HashSet<Pubkey>,
) {
    let Ok(target) = select_position(positions, selected, exited) else {
        log::info!("no position — nothing to report");
        return;
    };
    let mint = target.position.mint;
    let entries = fills.for_mint(&mint).await;
    if entries.is_empty() {
        log::info!("{mint}: no buys recorded in this session");
        log::info!("(the log is in-memory, so buys from a previous run are not listed)");
        return;
    }
    log::info!("{mint} — buy attempts, newest first:");
    for fill in &entries {
        let where_ = match (fill.slot, fill.delta) {
            (Some(slot), Some(delta)) => format!("slot={slot} delta={delta:+}"),
            _ => "not on chain".into(),
        };
        log::info!(
            "  #{} {} attempt={} {} {:.6} SOL {} {}",
            fill.buyer,
            fill.wallet,
            fill.attempt,
            fill.state.label(),
            lamports_to_sol(fill.lamports),
            where_,
            fill.signature
        );
    }
    let missing = fills.missing(&mint, cfg.buyers.len()).await;
    if missing.is_empty() {
        log::info!("all {} wallet(s) filled", cfg.buyers.len());
    } else {
        let list = missing
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        log::warn!(
            "{} of {} wallet(s) have NO landed buy: {list} — 'b {list} <pct> go' to fill them",
            missing.len(),
            cfg.buyers.len()
        );
    }
}

fn print_help() {
    for line in [
        "commands:",
        "  watch <creator>         snipe launches from this creator",
        "  unwatch <creator>       stop watching it",
        "  watching                list watched creators",
        "  wallets                 buyer wallets: index, SOL, tokens",
        "  positions               sniped positions on disk ('->' is the sell target)",
        "  select <mint>           pin which position 's' sells ('select auto' = newest)",
        "  status                  cost, live price, mcap, P&L (% and USD)",
        "  market                  live price / volume / net flow",
        "  s <pct>                 sell pct% of the position, ALL wallets  (simulate)",
        "  s <wallets> <pct>       sell pct% from chosen wallets          (simulate)",
        "  s <wallets> <pct> go    same, but ACTUALLY SEND",
        "  b <pct>                 buy with pct% of each wallet's SOL, ALL (simulate)",
        "  b <wallets> <pct>       buy from chosen wallets                (simulate)",
        "  b <wallets> <pct> go    same, but ACTUALLY SEND",
        "  fills                   per-wallet buy outcome for the target coin",
        "  <wallets> is 1 | 1,2 | 1-3 | all — same on both sides",
        "  price                   on-chain SOL/USD (Pyth)",
        "  help / quit",
        "notes: 'go' is the only thing that sends. Buys and sells run concurrently.",
        "       'b 100' spends the whole balance MINUS a reserve for fees and rent,",
        "       so it never builds a buy the wallet cannot pay for.",
        "       with several positions open, 's' targets the NEWEST still holding tokens",
        "       and names it before selling — 'select <mint>' to override.",
    ] {
        log::info!("{line}");
    }
}

async fn cmd_market(market: &Arc<RwLock<MarketTracker>>) {
    // Copy out and release: everything below is formatting, and the geyser
    // pipeline needs this lock for every trade it observes.
    let markets: Vec<(Pubkey, Market)> = {
        let m = market.read().await;
        m.tracked_mints()
            .into_iter()
            .filter_map(|mint| m.get(&mint).cloned().map(|mk| (mint, mk)))
            .collect()
    };
    if markets.is_empty() {
        log::info!("no tracked mints — a snipe starts tracking automatically");
    }
    for (mint, mk) in markets {
        if !mk.has_data() {
            log::info!("  {mint}  (no trades observed yet)");
            continue;
        }
        // One record per line: multi-line output written straight to stdout
        // staircases under raw mode and corrupts ratatui's back buffer, so
        // everything the console says goes through `log::` into the panel.
        log::info!("  {mint}");
        log::info!(
            "    price {:.10} SOL/token   mcap {:.2} SOL",
            mk.price_sol_per_token(),
            mk.market_cap_sol()
        );
        log::info!(
            "    in {:.4} SOL / out {:.4} SOL   net {:+.4}   {} buys / {} sells   last trade {}s ago",
            mk.volume_in_lamports as f64 / 1e9,
            mk.volume_out_lamports as f64 / 1e9,
            mk.net_flow_lamports() as f64 / 1e9,
            mk.buys,
            mk.sells,
            mk.age_secs()
        );
    }
}

async fn cmd_wallets(cfg: &Config, rpc: &Arc<RpcClient>, mint: Option<Pubkey>) {
    // Two round trips per wallet, awaited serially, is a frozen console for a
    // minute at 30 buyers. The calls are independent; run them together.
    let rows = join_all(cfg.buyers.iter().enumerate().map(|(i, buyer)| {
        let pk = buyer.keypair.pubkey();
        async move {
            let sol = rpc.get_balance(&pk).await.unwrap_or(0) as f64 / 1e9;
            let tokens = match mint {
                Some(m) => {
                    let ata = crate::pump::pdas::associated_token_address_with_program(
                        &pk,
                        &m,
                        &crate::pump::pdas::TOKEN_2022_PROGRAM_ID,
                    );
                    rpc.get_token_account_balance(&ata)
                        .await
                        .map(|b| b.ui_amount_string)
                        .unwrap_or_else(|_| "0".into())
                }
                None => "-".into(),
            };
            (i, pk, sol, tokens)
        }
    }))
    .await;
    log::info!("idx  wallet                                        SOL         tokens");
    for (i, pk, sol, tokens) in rows {
        log::info!("{i:>3}  {pk}  {sol:>9.6}  {tokens:>18}");
    }
}

async fn cmd_status(
    cfg: &Config,
    rpc: &Arc<RpcClient>,
    market: &Arc<RwLock<MarketTracker>>,
    positions: &[Position],
    target: Option<Pubkey>,
    usd: Option<f64>,
) {
    if positions.is_empty() {
        log::info!("no positions yet — nothing sniped in this working directory");
        return;
    }
    // Snapshot the markets and release the lock before any RPC work. Tokio's
    // RwLock is fair, so a writer queued behind this reader blocks every later
    // reader too: holding it across a dozen sequential round trips stalls the
    // geyser pipeline's `market.write()` on every trade event for as long as
    // `status` runs. Typing `status` must not cost a snipe.
    let markets: HashMap<Pubkey, Market> = {
        let m = market.read().await;
        positions
            .iter()
            .filter_map(|p| m.get(&p.mint).cloned().map(|mk| (p.mint, mk)))
            .collect()
    };
    for p in positions {
        log::info!(
            "position {} {}  creator {}",
            p.mint,
            if Some(p.mint) == target { "(target)" } else { "" },
            p.creator
        );
        let units = join_all(cfg.buyers.iter().map(|buyer| {
            let ata = crate::pump::pdas::associated_token_address_with_program(
                &buyer.keypair.pubkey(),
                &p.mint,
                &crate::pump::pdas::TOKEN_2022_PROGRAM_ID,
            );
            async move {
                rpc.get_token_account_balance(&ata)
                    .await
                    .ok()
                    .and_then(|b| b.amount.parse::<u64>().ok())
                    .unwrap_or(0)
            }
        }))
        .await
        .into_iter()
        .fold(0u64, u64::saturating_add);

        let cost = cost_basis(rpc, p).await;
        match &cost {
            Some(c) if c.is_complete() => {
                log::info!("  cost {:.6} SOL   holding {units} units", c.sol())
            }
            Some(c) => log::warn!(
                "  cost >= {:.6} SOL from {}/{} buys — PARTIAL, P&L withheld   holding {units} units",
                c.sol(),
                c.counted,
                c.recorded
            ),
            None => log::info!("  cost unavailable   holding {units} units"),
        }
        // Value comes from the reserves the last TradeEvent published — no
        // bonding-curve model involved.
        match markets.get(&p.mint).filter(|m| m.has_data()) {
            Some(m) => {
                let value = m.value_lamports(units) / 1e9;
                log::info!(
                    "  price {:.10} SOL/tok  mcap {:.2} SOL  value {value:.6} SOL",
                    m.price_sol_per_token(),
                    m.market_cap_sol()
                );
                // Only a complete basis produces a P&L. An understated cost
                // overstates the gain, which is the direction that talks an
                // operator out of an exit.
                if let Some(c) = cost.as_ref().filter(|c| c.is_complete()) {
                    let cost_sol = c.sol();
                    let pnl = value - cost_sol;
                    let pct = if cost_sol > 0.0 { pnl / cost_sol * 100.0 } else { 0.0 };
                    match usd {
                        Some(u) => {
                            log::info!("  PNL {pnl:+.6} SOL  {pct:+.2}%  (${:+.2})", pnl * u)
                        }
                        None => log::info!("  PNL {pnl:+.6} SOL  {pct:+.2}%"),
                    }
                }
            }
            None => log::info!("  no trades observed yet — value unavailable"),
        }
    }
}

/// Draw the panel and collect one command.
///
/// ratatui keeps a back buffer and emits only changed cells, so a live refresh
/// does not flicker — a manual clear-and-repaint at this cadence visibly blinks.
/// Returns `None` on Ctrl-C / Esc.
async fn read_line_with_panel(
    input: &mut String,
    snapshot: &Arc<RwLock<dashboard::Snapshot>>,
    market: &Arc<RwLock<MarketTracker>>,
    watched: &Arc<RwLock<HashSet<Pubkey>>>,
    logs: &LogRing,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
) -> Option<String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::{
        layout::{Constraint, Direction, Layout},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, Paragraph},
    };
    loop {
        {
            // Cheap in-memory read so watch/unwatch show on the next frame.
            let live: Vec<Pubkey> = watched.read().await.iter().copied().collect();
            snapshot.write().await.watching = live;
        }
        let snap = snapshot.read().await.clone();
        let mk = dashboard::market_for(market, snap.mint).await;
        let body = dashboard::render(&snap, mk.as_ref());
        let title = if snap.live { " sniper — LIVE " } else { " sniper — dry run " };
        let title_style = if snap.live {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Green)
        };

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(body.len() as u16 + 2),
                    Constraint::Min(3),
                    Constraint::Length(3),
                ])
                .split(f.area());

            let lines: Vec<Line> = body
                .iter()
                .map(|l| {
                    let style = if l.contains("PNL") {
                        if l.contains('-') && !l.contains('+') {
                            Style::default().fg(Color::Red)
                        } else {
                            Style::default().fg(Color::Green)
                        }
                    } else if l.starts_with("──") {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default()
                    };
                    Line::from(Span::styled(l.clone(), style))
                })
                .collect();
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(Span::styled(title, title_style)),
                ),
                chunks[0],
            );

            // Command output and pipeline events both land here, so nothing
            // needs to leave the panel to be readable.
            let rows = chunks[1].height.saturating_sub(2) as usize;
            let events: Vec<Line> = logs
                .tail(rows)
                .into_iter()
                .map(|(level, line)| {
                    let style = match level {
                        log::Level::Error => Style::default().fg(Color::Red),
                        log::Level::Warn => Style::default().fg(Color::Yellow),
                        _ if line.contains("SNIPE")
                            || line.contains("LANDED")
                            || line.contains("REALISED")
                            || line.contains("SOLD") =>
                        {
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                        }
                        _ => Style::default().fg(Color::Gray),
                    };
                    Line::from(Span::styled(line, style))
                })
                .collect();
            f.render_widget(
                Paragraph::new(events)
                    .block(Block::default().borders(Borders::ALL).title(" events ")),
                chunks[1],
            );

            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("sniper> ", Style::default().fg(Color::Cyan)),
                    Span::raw(input.as_str()),
                    Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
                ]))
                .block(Block::default().borders(Borders::ALL).title(" command ")),
                chunks[2],
            );
        });

        let pending = tokio::task::spawn_blocking(|| {
            event::poll(std::time::Duration::from_millis(250))
                .ok()
                .filter(|ready| *ready)
                .and_then(|_| event::read().ok())
        })
        .await
        .ok()
        .flatten();
        let Some(Event::Key(key)) = pending else {
            continue;
        };
        if !matches!(key.kind, KeyEventKind::Press) {
            continue;
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return None,
            KeyCode::Esc => return None,
            KeyCode::Enter => return Some(std::mem::take(input)),
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(mint: Pubkey, slot: Option<u64>) -> Position {
        Position {
            mint,
            creator: Pubkey::new_unique(),
            bonding_curve: Pubkey::new_unique(),
            buy_signatures: Vec::new(),
            create_slot: slot,
        }
    }

    fn parse(line: &str) -> Result<SellCommand, String> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        parse_sell(&parts)
    }

    #[test]
    fn a_bare_percentage_targets_every_wallet() {
        assert_eq!(
            parse("s 10").unwrap(),
            SellCommand {
                wallets: WalletSel::All,
                pct: 10.0,
                execute: false
            }
        );
        assert_eq!(
            parse("s 10 go").unwrap(),
            SellCommand {
                wallets: WalletSel::All,
                pct: 10.0,
                execute: true
            }
        );
    }

    #[test]
    fn two_numbers_are_wallet_then_percentage() {
        assert_eq!(
            parse("s 1 10").unwrap(),
            SellCommand {
                wallets: WalletSel::Only(vec![1]),
                pct: 10.0,
                execute: false
            }
        );
        assert_eq!(
            parse("s 3 100 go").unwrap(),
            SellCommand {
                wallets: WalletSel::Only(vec![3]),
                pct: 100.0,
                execute: true
            }
        );
    }

    fn parse_b(line: &str) -> Result<BuyCommand, String> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        parse_buy(&parts)
    }

    #[test]
    fn a_comma_list_selects_those_wallets() {
        assert_eq!(
            parse("s 1,2 10").unwrap().wallets,
            WalletSel::Only(vec![1, 2])
        );
        assert_eq!(
            parse_b("b 1,2,3,4 100 go").unwrap().wallets,
            WalletSel::Only(vec![1, 2, 3, 4])
        );
    }

    #[test]
    fn a_range_expands_inclusively() {
        assert_eq!(
            parse("s 1-3 50").unwrap().wallets,
            WalletSel::Only(vec![1, 2, 3])
        );
        // Mixed forms, because an operator will write both.
        assert_eq!(
            parse_b("b 0,2-4 25").unwrap().wallets,
            WalletSel::Only(vec![0, 2, 3, 4])
        );
    }

    #[test]
    fn duplicates_collapse() {
        // Two sells for one wallet in a single batch would both read the same
        // pre-sell balance, so the wallet would sell the percentage twice.
        assert_eq!(
            parse("s 2,1,2,1-2 50").unwrap().wallets,
            WalletSel::Only(vec![1, 2])
        );
    }

    #[test]
    fn all_is_spelled_out_or_implied() {
        assert_eq!(parse("s 10").unwrap().wallets, WalletSel::All);
        assert_eq!(parse("s all 10").unwrap().wallets, WalletSel::All);
        assert_eq!(parse_b("b ALL 100 go").unwrap().wallets, WalletSel::All);
    }

    #[test]
    fn buy_and_sell_parse_the_same_wallet_words() {
        // One grammar. If these ever diverge, `1,2` means different wallets on
        // the two sides and an operator has no way to know.
        for line in ["1,2 10", "1-3 10", "all 10", "2 10"] {
            let sell = parse(&format!("s {line}")).unwrap();
            let buy = parse_b(&format!("b {line}")).unwrap();
            assert_eq!(sell.wallets, buy.wallets, "diverged on '{line}'");
            assert_eq!(sell.pct, buy.pct, "diverged on '{line}'");
        }
    }

    #[test]
    fn an_out_of_range_wallet_fails_the_whole_command() {
        // Not a filter. An operator who believes there are ten wallets and
        // types `1,2,9` must not have two of them sold and be told it worked.
        let err = WalletSel::Only(vec![1, 2, 9])
            .resolve(4)
            .expect_err("9 is out of range");
        assert!(err.contains("out of range"), "{err}");
        assert_eq!(WalletSel::Only(vec![1, 2]).resolve(4).unwrap(), vec![1, 2]);
        assert_eq!(WalletSel::All.resolve(3).unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn a_backwards_range_is_refused() {
        let err = parse("s 3-1 50").expect_err("a backwards range must be refused");
        assert!(err.contains("backwards"), "{err}");
    }

    #[test]
    fn a_negative_wallet_index_is_refused_not_saturated() {
        // The bug this replaces parsed the index as f64 and cast it with
        // `as usize`, which SATURATES: `s -1 50 go` resolved to wallet 0 and
        // sold a wallet the operator never named.
        let err = parse("s -1 50 go").expect_err("a negative wallet index must be refused");
        assert!(err.contains("whole number"), "{err}");
    }

    #[test]
    fn a_non_integer_wallet_index_is_refused_not_truncated() {
        // `1.9 as usize` is 1 — a silently different wallet.
        assert!(parse("s 1.9 50").is_err());
        assert!(parse("s 1e2 50").is_err());
    }

    #[test]
    fn nan_and_inf_wallet_indexes_are_refused() {
        // Both parse cleanly as f64 and both cast to 0.
        for line in ["s nan 50 go", "s inf 50 go", "s -inf 50 go", "s NaN 50"] {
            assert!(parse(line).is_err(), "{line} must be refused");
        }
    }

    #[test]
    fn nan_and_inf_percentages_are_refused() {
        for line in ["s nan", "s inf", "s 1 nan", "s 1 inf go"] {
            let err = parse(line).expect_err("{line} must be refused");
            assert!(err.contains("number"), "{line}: {err}");
        }
    }

    #[test]
    fn percentages_outside_zero_to_one_hundred_are_refused() {
        assert!(parse("s 101").is_err());
        assert!(parse("s -5").is_err());
        assert!(parse("s 1 100.1").is_err());
        // The boundaries themselves are valid.
        assert!(parse("s 0").is_ok());
        assert!(parse("s 100 go").is_ok());
    }

    #[test]
    fn a_missing_or_overlong_argument_list_is_refused() {
        assert!(parse("s").is_err());
        assert!(parse("s go").is_err());
        assert!(parse("s 1 2 3").is_err());
    }

    #[test]
    fn positions_are_ordered_newest_first_and_deterministically() {
        // read_dir order is arbitrary, so without this the panel could show one
        // mint while a sell hit another.
        let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());
        let mut one = vec![position(a, Some(1)), position(b, Some(9))];
        let mut two = vec![position(b, Some(9)), position(a, Some(1))];
        sort_positions(&mut one);
        sort_positions(&mut two);
        assert_eq!(one[0].mint, b);
        assert_eq!(
            one.iter().map(|p| p.mint).collect::<Vec<_>>(),
            two.iter().map(|p| p.mint).collect::<Vec<_>>()
        );
    }

    #[test]
    fn positions_without_a_slot_still_order_deterministically() {
        let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());
        let (lo, hi) = if a.to_string() < b.to_string() { (a, b) } else { (b, a) };
        let mut ps = vec![position(hi, None), position(lo, None)];
        sort_positions(&mut ps);
        assert_eq!(ps[0].mint, lo);
    }

    /// Nothing known to be exited.
    fn live() -> HashSet<Pubkey> {
        HashSet::new()
    }

    #[test]
    fn a_single_position_needs_no_selection() {
        let m = Pubkey::new_unique();
        let ps = vec![position(m, Some(1))];
        let chosen = select_position(&ps, None, &live()).unwrap();
        assert_eq!(chosen.position.mint, m);
        // Nothing to disambiguate, so nothing to say.
        assert!(chosen.note.is_none());
    }

    #[test]
    fn the_newest_position_is_the_default_target() {
        // Snipe, then sell THAT coin: stale files from earlier snipes must not
        // stand between the operator and the exit.
        let ps = vec![
            position(Pubkey::new_unique(), Some(9)),
            position(Pubkey::new_unique(), Some(2)),
            position(Pubkey::new_unique(), Some(1)),
        ];
        let newest = ps[0].mint;
        assert_eq!(select_position(&ps, None, &live()).unwrap().position.mint, newest);
    }

    #[test]
    fn a_defaulted_target_names_the_mint_and_the_others() {
        // The original bug was a SILENT arbitrary pick. Choosing is fine;
        // choosing without saying which is not.
        let ps = vec![
            position(Pubkey::new_unique(), Some(2)),
            position(Pubkey::new_unique(), Some(1)),
        ];
        let chosen = select_position(&ps, None, &live()).unwrap();
        let note = chosen.note.expect("a defaulted target must report itself");
        assert!(note.contains(&chosen.position.mint.to_string()), "{note}");
        assert!(note.contains('1'), "must say how many others are open: {note}");
        assert!(note.contains("select"), "{note}");
    }

    #[test]
    fn an_exited_position_is_not_preferred_over_a_live_one() {
        // The newest file is a coin every wallet already sold out of — exactly
        // the stale record that caused the friction.
        let ps = vec![
            position(Pubkey::new_unique(), Some(9)),
            position(Pubkey::new_unique(), Some(2)),
        ];
        let (sold, held) = (ps[0].mint, ps[1].mint);
        let exited = HashSet::from([sold]);
        let chosen = select_position(&ps, None, &exited).unwrap();
        assert_eq!(chosen.position.mint, held);
        let note = chosen.note.expect("a defaulted target must report itself");
        assert!(note.contains("exited"), "{note}");
    }

    #[test]
    fn every_position_exited_still_yields_a_target() {
        // A sell is never blocked on selection — even when the scan says there
        // is nothing left anywhere, the newest position is still the answer.
        let ps = vec![
            position(Pubkey::new_unique(), Some(9)),
            position(Pubkey::new_unique(), Some(2)),
        ];
        let newest = ps[0].mint;
        let exited = ps.iter().map(|p| p.mint).collect();
        assert_eq!(select_position(&ps, None, &exited).unwrap().position.mint, newest);
    }

    #[test]
    fn an_explicit_selection_wins_over_order() {
        let ps = vec![
            position(Pubkey::new_unique(), Some(2)),
            position(Pubkey::new_unique(), Some(1)),
        ];
        let older = ps[1].mint;
        let chosen = select_position(&ps, Some(older), &live()).unwrap();
        assert_eq!(chosen.position.mint, older);
        // The operator named it, so there is nothing to disclose.
        assert!(chosen.note.is_none());
    }

    #[test]
    fn an_explicit_selection_of_an_exited_position_is_honoured() {
        // Balances are read from a scan up to 5s old, and a dust remainder is a
        // real reason to name a position the scan called empty. Never override.
        let ps = vec![
            position(Pubkey::new_unique(), Some(9)),
            position(Pubkey::new_unique(), Some(2)),
        ];
        let older = ps[1].mint;
        let exited = HashSet::from([older]);
        assert_eq!(
            select_position(&ps, Some(older), &exited).unwrap().position.mint,
            older
        );
    }

    #[test]
    fn a_selection_that_left_disk_is_an_error_not_a_fallback() {
        // Falling back to "some other position" here would sell a coin the
        // operator did not choose.
        let ps = vec![position(Pubkey::new_unique(), Some(1))];
        assert!(select_position(&ps, Some(Pubkey::new_unique()), &live()).is_err());
    }

    #[test]
    fn no_positions_is_an_error() {
        assert!(select_position(&[], None, &live()).is_err());
    }

    #[test]
    fn the_panel_follows_the_defaulted_target() {
        // dashboard::refresh reads positions.first(), and the refresh task
        // resolves through select_position — so a default that skipped a stale
        // position must still be what the panel shows.
        let ps = vec![
            position(Pubkey::new_unique(), Some(9)),
            position(Pubkey::new_unique(), Some(2)),
        ];
        let exited = HashSet::from([ps[0].mint]);
        let resolved = select_position(&ps, None, &exited).unwrap().position.mint;
        let ordered = order_for_target(ps, Some(resolved));
        assert_eq!(ordered[0].mint, resolved);
    }

    #[test]
    fn the_target_is_moved_to_the_front_for_the_panel() {
        // dashboard::refresh reads positions.first(), so this is what keeps the
        // panel and the sell path pointed at the same mint.
        let ps = vec![
            position(Pubkey::new_unique(), Some(3)),
            position(Pubkey::new_unique(), Some(2)),
            position(Pubkey::new_unique(), Some(1)),
        ];
        let (second, third) = (ps[1].mint, ps[2].mint);
        let ordered = order_for_target(ps, Some(third));
        assert_eq!(ordered[0].mint, third);
        // The rest keep their relative order.
        assert_eq!(ordered[2].mint, second);
    }

    #[test]
    fn a_partial_cost_basis_is_distinguishable() {
        let complete = CostBasis {
            lamports: 1_000_000,
            counted: 3,
            recorded: 3,
        };
        let partial = CostBasis {
            lamports: 1_000_000,
            counted: 2,
            recorded: 3,
        };
        assert!(complete.is_complete());
        assert!(!partial.is_complete());
        assert!((complete.sol() - 0.001).abs() < 1e-9);
    }
}
