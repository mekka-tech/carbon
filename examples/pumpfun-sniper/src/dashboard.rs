//! Live dashboard state.
//!
//! Split from rendering so the expensive half (RPC: balances, cost basis) runs
//! on a slow cadence while the cheap half (price, volume — already in memory
//! from `TradeEvent`s) can be redrawn every second. Polling four wallets over
//! RPC at 1 Hz would rate-limit the endpoint the hot path depends on.
//!
//! The slow half also polls the bonding curve account, because "already in
//! memory from `TradeEvent`s" is not true on every feed — see
//! `market::MarketTracker::apply_curve_snapshot` for why. That read belongs
//! here and nowhere else: it is RPC, so it must stay off the 1 Hz redraw and
//! far away from the dispatch hot path.

use {
    crate::{config::Config, console::Position, market::MarketTracker, pump::pdas},
    carbon_pumpfun_decoder::accounts::bonding_curve::BondingCurve,
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    std::sync::{Arc, OnceLock},
    tokio::sync::RwLock,
};

#[derive(Debug, Clone, Default)]
pub struct WalletRow {
    pub index: usize,
    pub pubkey: Pubkey,
    pub sol: f64,
    pub token_units: u64,
}

/// Everything the renderer needs, refreshed off the hot path.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Creators currently being sniped. Read from the shared set on every
    /// redraw, so `watch` / `unwatch` show up immediately — this is the first
    /// thing to confirm while waiting for a launch.
    pub watching: Vec<Pubkey>,
    /// False when SEND_MODE leaves the sniper in dry run.
    pub live: bool,
    pub feed: String,
    pub routes: String,
    pub buy_sol: f64,
    pub wallets: Vec<WalletRow>,
    pub mint: Option<Pubkey>,
    pub cost_lamports: Option<i128>,
    pub sol_usd: Option<f64>,
    pub last_refresh_unix: i64,
    pub refreshing: bool,
}

impl Snapshot {
    pub fn total_units(&self) -> u64 {
        self.wallets.iter().map(|w| w.token_units).sum()
    }
    pub fn total_sol(&self) -> f64 {
        self.wallets.iter().map(|w| w.sol).sum()
    }
}

/// The tracker the refresh task pushes polled curve reserves into.
///
/// WHY A PROCESS-WIDE HANDLE INSTEAD OF AN ARGUMENT: `refresh` is driven by a
/// task in `console::run` that is not given the tracker, and the natural fix —
/// one more parameter — is a change to `console.rs`. `market_for` is already
/// called with the tracker on every rendered frame, so it registers it here and
/// `refresh` picks it up from the next tick onwards. Collapse this into a
/// parameter on `refresh` the moment `console.rs` can take the edit; nothing
/// else depends on the indirection.
static POLL_TRACKER: OnceLock<Arc<RwLock<MarketTracker>>> = OnceLock::new();

/// Give the refresh task somewhere to publish polled curve reserves. Idempotent
/// — the first caller wins, and every caller passes the same process-wide
/// tracker.
pub fn attach_tracker(tracker: &Arc<RwLock<MarketTracker>>) {
    let _ = POLL_TRACKER.set(Arc::clone(tracker));
}

/// Virtual `(sol, token)` reserves out of a raw pump `BondingCurve` account.
///
/// Verified against the live account for
/// `G9xLug8eKE4dNBNPG3qJNPP7XBZ6mnbyNjSPxxeEpump` (a `create_v2` launch) — see
/// `decodes_a_real_onchain_curve_account`; the generated decoder's layout
/// matches byte for byte, including the trailing `quote_mint`, so nothing here
/// parses by offset.
fn curve_reserves(data: &[u8]) -> Option<(u64, u64)> {
    let curve = BondingCurve::decode(data)?;
    // The v2 curve names its SOL side `virtual_quote_reserves` and is in
    // principle free to be quoted in some other mint. Every consumer treats
    // this number as lamports, so refuse a non-SOL quote rather than publish a
    // price that is silently in the wrong unit. Real v2 SOL curves store the
    // default pubkey here (observed on-chain), not the WSOL mint; accept both.
    let quote_is_sol = curve.quote_mint == Pubkey::default() || curve.quote_mint == pdas::WSOL_MINT;
    if !quote_is_sol {
        return None;
    }
    // Zero on either side is a dead or not-yet-initialised curve; the tracker
    // rejects it anyway, but there is no point taking the write lock for it.
    if curve.virtual_quote_reserves == 0 || curve.virtual_token_reserves == 0 {
        return None;
    }
    Some((curve.virtual_quote_reserves, curve.virtual_token_reserves))
}

/// Read the position's bonding curve over RPC and hand its reserves to the
/// tracker.
///
/// The curve address comes off the position record written at snipe time, so
/// this never re-derives a PDA and never guesses which of the v1/v2 seeds
/// applied. Failures are silent by design: this is a fallback on a 5s timer,
/// and an endpoint hiccup would otherwise log once per tick forever.
///
/// Note a completed (migrated) curve keeps returning its final frozen reserves.
/// They are still the last honest curve price; AMM pricing after migration is
/// out of scope here.
async fn poll_bonding_curve(rpc: &RpcClient, position: &Position) {
    let Some(tracker) = POLL_TRACKER.get() else {
        return;
    };
    let Ok(account) = rpc.get_account(&position.bonding_curve).await else {
        return;
    };
    let Some((sol, tokens)) = curve_reserves(&account.data) else {
        return;
    };
    let now = now_unix();
    let mut tracker = tracker.write().await;
    // Tracking gates every write into the tracker, and it is only ever set by
    // the live snipe path. So after a restart the position sitting on disk is
    // untracked and this poll — and any trade event for it — is dropped on the
    // floor. The panel is showing this mint; that is what tracked means.
    // Idempotent, and bounded by the positions on disk.
    tracker.track(position.mint);
    tracker.apply_curve_snapshot(position.mint, sol, tokens, now);
}

/// Local wall clock as a unix timestamp.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Refresh the RPC-backed half of the snapshot.
pub async fn refresh(
    cfg: &Config,
    rpc: &Arc<RpcClient>,
    positions: &[Position],
    snapshot: &Arc<RwLock<Snapshot>>,
) {
    {
        snapshot.write().await.refreshing = true;
    }
    let mint = positions.first().map(|p| p.mint);
    let mut wallets = Vec::with_capacity(cfg.buyers.len());
    for (i, buyer) in cfg.buyers.iter().enumerate() {
        let pk = buyer.keypair.pubkey();
        let sol = rpc.get_balance(&pk).await.unwrap_or(0) as f64 / 1e9;
        let token_units = match mint {
            Some(m) => {
                let ata = crate::pump::pdas::associated_token_address_with_program(
                    &pk,
                    &m,
                    &crate::pump::pdas::TOKEN_2022_PROGRAM_ID,
                );
                rpc.get_token_account_balance(&ata)
                    .await
                    .ok()
                    .and_then(|b| b.amount.parse::<u64>().ok())
                    .unwrap_or(0)
            }
            None => 0,
        };
        wallets.push(WalletRow {
            index: i,
            pubkey: pk,
            sol,
            token_units,
        });
    }
    let cost = match positions.first() {
        Some(p) => crate::console::cost_basis_lamports(rpc, p).await,
        None => None,
    };
    let usd = crate::sell::sol_usd(rpc).await;
    // The market panel's fallback price source. Only meaningful for the mint
    // the panel is showing, which is `positions.first()` — the same one `mint`
    // above comes from.
    if let Some(p) = positions.first() {
        poll_bonding_curve(rpc, p).await;
    }
    let now = now_unix();

    let mut s = snapshot.write().await;
    s.wallets = wallets;
    s.mint = mint;
    s.cost_lamports = cost;
    s.sol_usd = usd;
    // Recomputed every refresh, not captured once at console start: in
    // BUY_SIZING=balance the per-wallet sizes are re-derived after each
    // confirmed snipe and sell, so a value read at startup goes stale the
    // first time anything lands.
    s.buy_sol = cfg
        .buyers
        .iter()
        .map(|b| b.buy_amount_lamports())
        .fold(0u64, u64::saturating_add) as f64
        / 1e9;
    s.last_refresh_unix = now;
    s.refreshing = false;
}

fn short(pk: &Pubkey) -> String {
    let s = pk.to_string();
    format!("{}…{}", &s[..4], &s[s.len().saturating_sub(4)..])
}

/// Render the panel as lines. Pure so it can be unit-tested without a terminal.
pub fn render(snapshot: &Snapshot, market: Option<&crate::market::Market>) -> Vec<String> {
    let w: usize = 78;
    let bar = |title: &str| {
        let t = format!("── {title} ");
        format!("{t}{}", "─".repeat(w.saturating_sub(t.chars().count())))
    };
    let mut out = Vec::new();
    let usd = snapshot.sol_usd;
    let fmt_usd = |sol: f64| match usd {
        Some(u) => format!(" (${:.2})", sol * u),
        None => String::new(),
    };

    out.push(bar("SNIPER"));
    if snapshot.watching.is_empty() {
        out.push("  watching  NOTHING — use 'watch <creator>'".into());
    } else {
        for (i, c) in snapshot.watching.iter().enumerate() {
            out.push(format!(
                "  {}  {c}",
                if i == 0 { "watching" } else { "        " }
            ));
        }
    }
    out.push(format!(
        "  feed {}   routes {}   buy {:.3} SOL total across {}   {}",
        snapshot.feed,
        snapshot.routes,
        snapshot.buy_sol,
        snapshot.wallets.len(),
        if snapshot.live { "*** LIVE ***" } else { "dry run" }
    ));

    out.push(bar("POSITION"));
    match snapshot.mint {
        None => out.push("  no position — waiting for a snipe".into()),
        Some(mint) => {
            out.push(format!("  mint  {mint}"));
            let units = snapshot.total_units();
            let cost_sol = snapshot.cost_lamports.map(|c| c as f64 / 1e9);
            // `has_data`, not `last_update_unix > 0`: a market can hold valid
            // reserves under a zero/skewed timestamp, and a polled one is keyed
            // off a local clock entirely.
            match market.filter(|m| m.has_data()) {
                Some(m) => {
                    let value = m.value_lamports(units) / 1e9;
                    // Market cap the way pump.fun and the aggregators quote it:
                    // price × the full 1B supply, headlined in USD. Showing
                    // only the SOL figure means comparing against every other
                    // screen the operator has open requires mental arithmetic
                    // against a SOL price that moves.
                    out.push(format!(
                        "  price {:.10} SOL/tok    mcap {:>8.2} SOL{}    {} {:>3}s ago",
                        m.price_sol_per_token(),
                        m.market_cap_sol(),
                        fmt_usd(m.market_cap_sol()),
                        m.source_label(),
                        m.age_secs()
                    ));
                    match cost_sol {
                        Some(c) => {
                            let pnl = value - c;
                            let pct = if c > 0.0 { pnl / c * 100.0 } else { 0.0 };
                            out.push(format!(
                                "  cost  {c:>10.6} SOL{}     value {value:>10.6} SOL{}",
                                fmt_usd(c),
                                fmt_usd(value)
                            ));
                            out.push(format!(
                                "  PNL   {pnl:>+10.6} SOL{}     {pct:>+8.2}%",
                                fmt_usd(pnl)
                            ));
                        }
                        None => out.push(format!("  value {value:>10.6} SOL{}", fmt_usd(value))),
                    }
                }
                None => out.push("  price  no market data — no trades, no curve read".into()),
            }
        }
    }

    out.push(bar("MARKET"));
    // Volume and trade counts only exist if trade events reached us. A market
    // priced off a polled curve has structural zeros here, and printing them
    // would read as "nobody is trading" rather than "we cannot see trades".
    match market {
        Some(m) if m.has_trade_flow() => out.push(format!(
            "  in {:>9.4} SOL   out {:>9.4} SOL   net {:>+9.4}   {} buys / {} sells",
            m.volume_in_lamports as f64 / 1e9,
            m.volume_out_lamports as f64 / 1e9,
            m.net_flow_lamports() as f64 / 1e9,
            m.buys,
            m.sells
        )),
        Some(m) if m.has_data() => out.push(
            "  flow  UNKNOWN — price is polled from the bonding curve; \
             volume needs trade events"
                .into(),
        ),
        _ => out.push("  no trades observed yet".into()),
    }

    out.push(bar("WALLETS"));
    out.push("   #  wallet         SOL         tokens            value       pnl".into());
    let price = market.map(|m| m.price_lamports_per_unit()).unwrap_or(0.0);
    let per_wallet_cost = snapshot
        .cost_lamports
        .filter(|_| !snapshot.wallets.is_empty())
        .map(|c| c as f64 / snapshot.wallets.len() as f64);
    for row in &snapshot.wallets {
        let value = price * row.token_units as f64 / 1e9;
        let pnl = match per_wallet_cost {
            Some(c) if c > 0.0 => format!("{:>+7.2}%", (value - c / 1e9) / (c / 1e9) * 100.0),
            _ => "      -".into(),
        };
        out.push(format!(
            "  {:>2}  {}   {:>9.6}   {:>14}   {:>9.6}  {pnl}",
            row.index,
            short(&row.pubkey),
            row.sol,
            row.token_units,
            value
        ));
    }
    out.push(format!(
        "  total {:>9.6} SOL{}   across {} wallet(s){}",
        snapshot.total_sol(),
        fmt_usd(snapshot.total_sol()),
        snapshot.wallets.len(),
        if snapshot.refreshing { "  [refreshing]" } else { "" }
    ));
    out.push("─".repeat(w));
    out
}

/// Look up the tracked market for the snapshot's mint.
pub async fn market_for(
    tracker: &Arc<RwLock<MarketTracker>>,
    mint: Option<Pubkey>,
) -> Option<crate::market::Market> {
    // Every rendered frame passes the tracker through here, which is what lets
    // the refresh task find it. See `POLL_TRACKER`.
    attach_tracker(tracker);
    let m = tracker.read().await;
    mint.and_then(|mint| m.get(&mint).cloned())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::market::{Market, MarketSource},
    };

    /// The live `BondingCurve` account
    /// `F4bz7J6riCRR11r1z4ENY3DdA7AnYAco1VcbxFnmALPL` (curve for mint
    /// `G9xLug8eKE4dNBNPG3qJNPP7XBZ6mnbyNjSPxxeEpump`, a `create_v2` launch),
    /// fetched over RPC at slot 436241360 — 115 bytes. Kept verbatim so the
    /// decode is pinned to real chain bytes rather than to an assumed layout.
    const LIVE_CURVE_B64: &str = "F7f4N2DYrGAAENhH488DAAesI/wGAAAAAHjF+1HRAgAHAAAAAAAAAACAxqR+jQMAAFssXPqZ1xMiMN++dKhg00ZJTWj/YPwjimgvRYKVby/1AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";

    fn live_curve_bytes() -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(LIVE_CURVE_B64)
            .expect("fixture is valid base64")
    }

    #[test]
    fn decodes_a_real_onchain_curve_account() {
        let data = live_curve_bytes();
        assert_eq!(data.len(), 115, "fixture is not the account we captured");
        let (sol, tokens) = curve_reserves(&data).expect("live curve failed to decode");
        // 30 SOL virtual + the 7 lamports of real quote already on the curve,
        // against the standard 1.073e15 base units.
        assert_eq!(sol, 30_000_000_007);
        assert_eq!(tokens, 1_073_000_000_000_000);
    }

    #[test]
    fn a_polled_curve_prices_a_position_sensibly() {
        // End to end on real bytes: the numbers the panel would show.
        let (sol, tokens) = curve_reserves(&live_curve_bytes()).unwrap();
        let mint = Pubkey::new_unique();
        let mut tracker = crate::market::MarketTracker::default();
        tracker.track(mint);
        assert!(tracker.apply_curve_snapshot(mint, sol, tokens, 1));
        let market = tracker.get(&mint).unwrap();
        // A fresh curve is worth tens of SOL, not millions.
        assert!(market.market_cap_sol() > 10.0 && market.market_cap_sol() < 100.0);
    }

    #[test]
    fn garbage_account_data_is_rejected() {
        assert!(curve_reserves(&[]).is_none());
        assert!(curve_reserves(&[0u8; 115]).is_none());
    }

    #[test]
    fn a_polled_market_says_so_and_hides_the_volume_zeros() {
        // The whole point of the source label: 0 buys / 0 sells on a polled
        // market is a blind spot, not a measurement.
        let snap = Snapshot {
            mint: Some(Pubkey::new_unique()),
            ..Default::default()
        };
        let market = Market {
            virtual_sol_reserves: 30_000_000_007,
            virtual_token_reserves: 1_073_000_000_000_000,
            has_data: true,
            last_update_unix: 1,
            source: MarketSource::CurvePoll,
            ..Default::default()
        };
        let out = render(&snap, Some(&market)).join("\n");
        assert!(out.contains("curve poll"), "{out}");
        assert!(out.contains("UNKNOWN"), "{out}");
        assert!(!out.contains("0 buys"), "{out}");
    }

    #[test]
    fn a_trade_fed_market_still_reports_flow() {
        let snap = Snapshot {
            mint: Some(Pubkey::new_unique()),
            ..Default::default()
        };
        let market = Market {
            virtual_sol_reserves: 2,
            virtual_token_reserves: 1,
            volume_in_lamports: 500,
            buys: 1,
            has_data: true,
            last_update_unix: 1,
            source: MarketSource::TradeEvent,
            ..Default::default()
        };
        let out = render(&snap, Some(&market)).join("\n");
        assert!(out.contains("last trade"), "{out}");
        assert!(out.contains("1 buys"), "{out}");
    }

    #[test]
    fn renders_without_a_position() {
        let out = render(&Snapshot::default(), None);
        assert!(out.iter().any(|l| l.contains("no position")));
        assert!(out.iter().any(|l| l.contains("WALLETS")));
    }

    #[test]
    fn watched_creators_are_always_visible() {
        // The panel is what you stare at while waiting; if it does not show the
        // creator, there is no way to tell you are armed on the right target.
        let c = Pubkey::new_unique();
        let snap = Snapshot {
            watching: vec![c],
            live: true,
            ..Default::default()
        };
        let out = render(&snap, None).join("\n");
        assert!(out.contains(&c.to_string()), "{out}");
        assert!(out.contains("LIVE"), "{out}");
    }

    #[test]
    fn an_empty_watch_list_is_called_out_loudly() {
        let out = render(&Snapshot::default(), None).join("\n");
        assert!(out.contains("NOTHING"), "{out}");
    }

    #[test]
    fn pnl_is_shown_when_cost_and_price_are_known() {
        let snap = Snapshot {
            wallets: vec![WalletRow {
                index: 0,
                pubkey: Pubkey::new_unique(),
                sol: 0.4,
                token_units: 1_000_000,
            }],
            mint: Some(Pubkey::new_unique()),
            // cost 0.001 SOL against a position now worth 0.002 -> +100%
            cost_lamports: Some(1_000_000),
            sol_usd: Some(100.0),
            ..Default::default()
        };
        let market = Market {
            virtual_sol_reserves: 2,
            virtual_token_reserves: 1,
            has_data: true,
            last_update_unix: 1,
            ..Default::default()
        };
        let out = render(&snap, Some(&market)).join("\n");
        assert!(out.contains("PNL"), "{out}");
        assert!(out.contains('%'), "{out}");
        // USD should appear once a price is known.
        assert!(out.contains('$'), "{out}");
    }

    #[test]
    fn usd_is_omitted_when_no_price_is_available() {
        let snap = Snapshot {
            wallets: vec![],
            sol_usd: None,
            ..Default::default()
        };
        assert!(!render(&snap, None).join("\n").contains('$'));
    }
}
