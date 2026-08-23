//! Live market state, built from pump.fun `TradeEvent` CPI events, with a
//! polled bonding-curve account as the fallback.
//!
//! Every pump trade emits a `TradeEvent` carrying the actual filled amounts
//! **and the curve's virtual reserves after the fill**. That removes the need to
//! model the bonding curve at all: price, market cap and position value come
//! from what the chain just published rather than from reserves we guessed.
//!
//! This is what makes honest unrealised P&L possible. The v2 quote model was
//! wrong by ~5.8x because `Global` carries no v2 token reserve; the trade event
//! carries the real one on every single trade.
//!
//! # Why `apply_curve_snapshot` exists — do not delete it as redundant
//!
//! `TradeEvent` is an **inner** instruction: the program emits it as an Anchor
//! self-CPI while the transaction executes, so carbon only ever recovers it
//! from `meta.inner_instructions`. The shredstream datasource has no meta — it
//! reconstructs transactions from shreds, i.e. from what was *submitted*, not
//! from what executing them produced, and leaves meta at
//! `..Default::default()`. On a shredstream-only feed **not one `TradeEvent`
//! ever arrives**, so nothing here is ever populated and price, market cap,
//! position value and unrealised P&L all render blank for as long as the
//! process runs. That is not a hypothetical: it is what an operator hit in
//! production.
//!
//! The proper fix is a second feed that carries meta (Yellowstone). Until an
//! endpoint for one exists, `apply_curve_snapshot` lets the dashboard's slow
//! RPC refresh read the bonding curve account directly and push its virtual
//! reserves in here, which needs nothing but the RPC endpoint the sniper
//! already has. It carries reserves and *only* reserves — see `MarketSource`.

use {
    solana_pubkey::Pubkey,
    std::collections::HashMap,
    std::time::{SystemTime, UNIX_EPOCH},
};

/// ASSUMPTION — NOT READ PER MINT.
///
/// Every figure below that is denominated in *whole tokens* (price per token,
/// market cap) assumes the classic pump.fun v1 mint shape: exactly 1e9 whole
/// tokens in existence at 6 decimals. Nothing here reads the mint account, so
/// nothing here verifies it.
///
/// When this breaks: **Token-2022 `create_v2` coins carry their own decimals**
/// in their mint (and a `create_v2` launch is free to use a different supply).
/// A coin at 9 decimals makes `price_sol_per_token` off by 1000x and
/// `market_cap_sol` off by the same factor in the other direction — silently,
/// with a plausible-looking number. Reserve-derived quantities
/// (`price_lamports_per_unit`, `value_lamports`, all volume/flow figures) are
/// in *base units* and are unaffected, so a position's SOL value stays honest
/// even when the per-token display is wrong.
///
/// The fix, when it matters: fetch the mint once at track time and store the
/// real decimals/supply on `Market`.
const ASSUMED_TOTAL_SUPPLY_TOKENS: f64 = 1_000_000_000.0;
/// Base units per whole token, i.e. 10^6 — see `ASSUMED_TOTAL_SUPPLY_TOKENS`.
const ASSUMED_BASE_UNITS_PER_TOKEN: f64 = 1_000_000.0;

/// How long trade-event state stays authoritative before a polled curve
/// snapshot is allowed to replace it.
///
/// Trade events are the better source *while they are arriving*: they are
/// per-fill, they carry volume, and on a shred feed they land before the block
/// is even confirmed — ahead of anything an RPC read can see. A poll that
/// overwrote them would move the price *backwards* to confirmed state and make
/// the panel claim polled data while a live feed was working fine. So the poll
/// only takes over once the trade feed has visibly gone quiet, which on a
/// shredstream-only run is immediately and forever.
///
/// Sized against the dashboard's 5s refresh: long enough that a normal gap
/// between trades does not flip the source back and forth, short enough that a
/// dead feed is covered within a few refresh ticks.
const TRADE_EVENT_AUTHORITY_SECS: i64 = 15;

/// Where a `Market`'s current reserves came from.
///
/// The panel has to be able to say this out loud: a polled snapshot supports a
/// price but carries no volume, no buy/sell counts and no per-fill history, so
/// presenting it identically to live trade data would turn structural zeros
/// into apparent measurements ("0 buys / 0 sells" reads as "nobody is trading",
/// not as "we cannot see trades").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MarketSource {
    /// Nothing observed yet, from either source.
    #[default]
    None,
    /// A pump `TradeEvent` — authoritative, per-fill, carries volume.
    TradeEvent,
    /// A direct read of the bonding curve account. Reserves only.
    CurvePoll,
}

#[derive(Debug, Clone, Default)]
pub struct Market {
    /// Virtual reserves as of the last trade — the curve state, from chain.
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    /// Lamports in / out across observed trades.
    pub volume_in_lamports: u128,
    pub volume_out_lamports: u128,
    pub buys: u64,
    pub sells: u64,
    /// Price implied by the most recent fill, lamports per base unit.
    pub last_fill_price: f64,
    /// Whether any trade has been folded in at all. This, not
    /// `last_update_unix`, is the "do we have data" question: a `TradeEvent`
    /// may legitimately carry `timestamp == 0` (or a clock-skewed value), and
    /// keying off the timestamp made such a market render forever as "no trades
    /// observed yet" despite holding valid reserves.
    pub has_data: bool,
    /// Timestamp of the newest observation folded in — a trade event's
    /// validator timestamp, or the local instant a curve poll was fetched.
    /// Normalised on the way in: a non-positive value is replaced with local
    /// wall-clock time, so this is always a real instant once `has_data` is
    /// set.
    pub last_update_unix: i64,
    /// Which source the reserves above came from. Never assume `TradeEvent`:
    /// on a shredstream-only feed it is `CurvePoll` for the whole run.
    pub source: MarketSource,
}

impl Market {
    /// Lamports per base token unit, from the live virtual reserves. Falls back
    /// to the last fill when reserves are absent.
    pub fn price_lamports_per_unit(&self) -> f64 {
        if self.virtual_token_reserves > 0 && self.virtual_sol_reserves > 0 {
            self.virtual_sol_reserves as f64 / self.virtual_token_reserves as f64
        } else {
            self.last_fill_price
        }
    }

    /// SOL per whole token. Assumes 6 decimals — see
    /// `ASSUMED_TOTAL_SUPPLY_TOKENS`.
    pub fn price_sol_per_token(&self) -> f64 {
        self.price_lamports_per_unit() * ASSUMED_BASE_UNITS_PER_TOKEN / 1e9
    }

    /// Market cap in SOL. Assumes a 1e9 whole-token supply — see
    /// `ASSUMED_TOTAL_SUPPLY_TOKENS`.
    pub fn market_cap_sol(&self) -> f64 {
        self.price_sol_per_token() * ASSUMED_TOTAL_SUPPLY_TOKENS
    }

    /// Whether this mint has *any* usable market state — from a trade event or
    /// from a curve poll. Callers should ask this rather than testing
    /// `last_update_unix > 0`.
    pub fn has_data(&self) -> bool {
        self.has_data
    }

    /// Whether the volume and buy/sell counters are real measurements.
    ///
    /// They can only ever come from trade events; a curve poll reads reserves
    /// and nothing else. Render must gate on this, otherwise a poll-fed market
    /// prints "0.0000 SOL in / 0 buys" — a structural blind spot dressed up as
    /// an observation of a dead market.
    pub fn has_trade_flow(&self) -> bool {
        self.buys > 0 || self.sells > 0
    }

    /// What `age_secs` is measuring, for the panel to print next to it.
    pub fn source_label(&self) -> &'static str {
        match self.source {
            MarketSource::None => "no data",
            MarketSource::TradeEvent => "last trade",
            MarketSource::CurvePoll => "curve poll",
        }
    }

    /// What `units` base units are worth right now, in lamports.
    pub fn value_lamports(&self, units: u64) -> f64 {
        self.price_lamports_per_unit() * units as f64
    }

    /// Net lamports flow — positive means more bought than sold.
    pub fn net_flow_lamports(&self) -> i128 {
        self.volume_in_lamports as i128 - self.volume_out_lamports as i128
    }

    /// Seconds since the last observation — trade or poll, see `source_label`.
    /// Never negative.
    ///
    /// `last_update_unix` comes from the validator that produced the event, so
    /// it can sit *ahead* of the local clock (leader clock skew, or a local
    /// clock that has not been disciplined yet). A raw subtraction then renders
    /// as "last trade -3s ago", which reads as a bug in the feed. Clamp at 0:
    /// "just now" is the honest answer for a future timestamp.
    pub fn age_secs(&self) -> i64 {
        if !self.has_data() {
            return 0;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        now.saturating_sub(self.last_update_unix).max(0)
    }
}

/// Local wall clock as a unix timestamp, used only to stamp events that arrive
/// without a usable one.
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Per-mint market state. Only mints passed to `track` accumulate, so the
/// tracker stays bounded while the feed carries the entire network.
/// `markets` is the tracking set: a mint is tracked iff it has an entry, which
/// `track` creates. An earlier version carried a parallel `HashMap<Pubkey, ()>`
/// — a `HashSet` in disguise, and one that could drift out of step with
/// `markets`. One map, one source of truth.
#[derive(Debug, Default)]
pub struct MarketTracker {
    markets: HashMap<Pubkey, Market>,
}

impl MarketTracker {
    pub fn track(&mut self, mint: Pubkey) {
        self.markets.entry(mint).or_default();
    }

    pub fn is_tracked(&self, mint: &Pubkey) -> bool {
        self.markets.contains_key(mint)
    }

    pub fn get(&self, mint: &Pubkey) -> Option<&Market> {
        self.markets.get(mint)
    }

    pub fn tracked_mints(&self) -> Vec<Pubkey> {
        self.markets.keys().copied().collect()
    }

    /// Fold one trade event into the market for its mint.
    ///
    /// Ordering: the feed does not guarantee it. Shredstream carries
    /// pre-confirmation shreds, so forked-off entries arrive and are never
    /// retracted; geyser can reorder across connections; a retry can replay an
    /// older trade. Reserves are *absolute curve state*, not a delta, so
    /// applying a stale event overwrites newer reserves with older ones and
    /// walks `last_update_unix` backwards — the panel then shows a price that
    /// jumps backwards for no visible reason. Events strictly older than the
    /// newest one seen are therefore dropped.
    ///
    /// Equal timestamps are *not* dropped: pump timestamps are whole seconds
    /// and a busy launch puts many genuine trades in the same second.
    ///
    /// Volume counters take the same drop, which is deliberate: a stale event
    /// is either a duplicate or a fork, and counting it inflates the totals
    /// either way.
    ///
    /// The guard is deliberately scoped to *trade-event* state. A curve poll
    /// stamps `last_update_unix` with the local fetch clock, which is not
    /// comparable to a validator timestamp and is generally ahead of it — so
    /// comparing against it would let the fallback silently swallow the real
    /// events it exists to stand in for. Trade events always win over a poll.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        mint: Pubkey,
        sol_amount: u64,
        token_amount: u64,
        is_buy: bool,
        virtual_sol_reserves: u64,
        virtual_token_reserves: u64,
        timestamp: i64,
    ) {
        // Only tracked mints accumulate — the feed carries the whole network.
        let Some(m) = self.markets.get_mut(&mint) else {
            return;
        };
        // A missing or nonsensical timestamp still describes a trade that just
        // happened, so stamp it locally rather than storing 0 and making the
        // market look dataless.
        let observed_at = if timestamp > 0 { timestamp } else { now_unix() };
        if m.source == MarketSource::TradeEvent && observed_at < m.last_update_unix {
            return;
        }
        if virtual_sol_reserves > 0 && virtual_token_reserves > 0 {
            m.virtual_sol_reserves = virtual_sol_reserves;
            m.virtual_token_reserves = virtual_token_reserves;
        }
        if token_amount > 0 {
            m.last_fill_price = sol_amount as f64 / token_amount as f64;
        }
        if is_buy {
            m.volume_in_lamports = m.volume_in_lamports.saturating_add(sol_amount as u128);
            m.buys = m.buys.saturating_add(1);
        } else {
            m.volume_out_lamports = m.volume_out_lamports.saturating_add(sol_amount as u128);
            m.sells = m.sells.saturating_add(1);
        }
        m.has_data = true;
        m.last_update_unix = observed_at;
        m.source = MarketSource::TradeEvent;
    }

    /// Apply virtual reserves read straight off the bonding curve account,
    /// i.e. from something other than a trade event.
    ///
    /// WHY THIS EXISTS: see the module header. On a shredstream-only feed
    /// `observe` is never called at all, because `TradeEvent` is an inner
    /// instruction and shreds carry no execution metadata. Without this the
    /// panel has no price, ever. Deleting it as "duplicated by `observe`"
    /// re-breaks that.
    ///
    /// Precedence, in order:
    /// 1. The mint must be tracked, and both reserves must be non-zero — a
    ///    failed or empty account read says nothing and must not blank a
    ///    working price.
    /// 2. Never walk `last_update_unix` backwards (same rule as `observe`).
    /// 3. Trade events outrank a poll for `TRADE_EVENT_AUTHORITY_SECS` after
    ///    the last one: while a real feed is live it is both more current and
    ///    richer, and a poll would drag the price back to confirmed state.
    ///
    /// Volume, buy/sell counts and `last_fill_price` are left untouched: the
    /// curve account simply does not contain them. `has_trade_flow` stays false
    /// so the panel can say so instead of printing zeros.
    ///
    /// Returns whether the snapshot was applied.
    pub fn apply_curve_snapshot(
        &mut self,
        mint: Pubkey,
        virtual_sol_reserves: u64,
        virtual_token_reserves: u64,
        fetched_at: i64,
    ) -> bool {
        let Some(m) = self.markets.get_mut(&mint) else {
            return false;
        };
        if virtual_sol_reserves == 0 || virtual_token_reserves == 0 {
            return false;
        }
        let at = if fetched_at > 0 { fetched_at } else { now_unix() };
        if m.has_data {
            if at <= m.last_update_unix {
                return false;
            }
            if m.source == MarketSource::TradeEvent
                && at.saturating_sub(m.last_update_unix) < TRADE_EVENT_AUTHORITY_SECS
            {
                return false;
            }
        }
        m.virtual_sol_reserves = virtual_sol_reserves;
        m.virtual_token_reserves = virtual_token_reserves;
        m.has_data = true;
        m.last_update_unix = at;
        m.source = MarketSource::CurvePoll;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mint() -> Pubkey {
        Pubkey::new_unique()
    }

    #[test]
    fn untracked_mints_are_ignored() {
        // The feed carries the whole network; without this the tracker would
        // grow unbounded.
        let mut t = MarketTracker::default();
        t.observe(mint(), 1_000, 1_000, true, 10, 10, 0);
        assert!(t.tracked_mints().is_empty());
    }

    #[test]
    fn price_comes_from_the_live_reserves() {
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        // 30 SOL against 1.073e15 base units — a fresh pump curve.
        t.observe(m, 1_000_000_000, 35_000_000_000_000, true, 30_000_000_000, 1_073_000_000_000_000, 1);
        let market = t.get(&m).unwrap();
        let price = market.price_sol_per_token();
        // ~2.8e-8 SOL per whole token on a fresh curve.
        assert!(price > 1e-8 && price < 1e-7, "{price}");
        // Market cap should land in the tens of SOL, not millions.
        assert!(market.market_cap_sol() > 10.0 && market.market_cap_sol() < 100.0);
    }

    #[test]
    fn buys_and_sells_accumulate_separately() {
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        t.observe(m, 500, 10, true, 1, 1, 1);
        t.observe(m, 200, 5, false, 1, 1, 2);
        let market = t.get(&m).unwrap();
        assert_eq!(market.volume_in_lamports, 500);
        assert_eq!(market.volume_out_lamports, 200);
        assert_eq!(market.net_flow_lamports(), 300);
        assert_eq!((market.buys, market.sells), (1, 1));
    }

    #[test]
    fn position_value_scales_with_holdings() {
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        t.observe(m, 1_000, 1_000, true, 1_000, 1_000, 1);
        let market = t.get(&m).unwrap();
        // reserves 1:1 -> 1 lamport per base unit
        assert!((market.value_lamports(5_000) - 5_000.0).abs() < 1.0);
    }

    #[test]
    fn a_stale_event_does_not_overwrite_newer_reserves() {
        // Reserves are absolute curve state, so a late-arriving older trade
        // would rewind the price if it were applied.
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        t.observe(m, 1_000, 1_000, true, 2_000, 1_000, 100);
        t.observe(m, 1_000, 1_000, true, 1_000, 1_000, 50);
        let market = t.get(&m).unwrap();
        assert_eq!(market.virtual_sol_reserves, 2_000, "stale reserves applied");
        assert_eq!(market.last_update_unix, 100, "clock walked backwards");
        assert_eq!(market.buys, 1, "stale trade counted");
    }

    #[test]
    fn same_second_trades_all_count() {
        // Pump timestamps are whole seconds; a busy launch legitimately puts
        // many trades in one. Dropping ties would silently lose most of them.
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        t.observe(m, 100, 10, true, 1, 1, 7);
        t.observe(m, 100, 10, true, 1, 1, 7);
        t.observe(m, 100, 10, false, 1, 1, 7);
        let market = t.get(&m).unwrap();
        assert_eq!((market.buys, market.sells), (2, 1));
    }

    #[test]
    fn a_zero_timestamp_still_produces_a_market_with_data() {
        // The old sentinel was `last_update_unix > 0`, so a trade carrying
        // timestamp 0 rendered forever as "no trades observed yet" even with
        // valid reserves behind it.
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        t.observe(m, 1_000, 1_000, true, 1_000, 1_000, 0);
        let market = t.get(&m).unwrap();
        assert!(market.has_data(), "market with reserves reported as dataless");
        assert!(market.last_update_unix > 0, "timestamp not normalised");
    }

    #[test]
    fn a_tracked_mint_with_no_trades_has_no_data() {
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        assert!(t.is_tracked(&m));
        assert!(!t.get(&m).unwrap().has_data());
    }

    #[test]
    fn a_curve_poll_fills_the_gap_when_no_trade_events_arrive() {
        // The shredstream case: `observe` is never called, so without the poll
        // this market would have no price for the whole run.
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        assert!(t.apply_curve_snapshot(m, 30_000_000_007, 1_073_000_000_000_000, 1_000));
        let market = t.get(&m).unwrap();
        assert!(market.has_data(), "poll did not produce usable state");
        assert_eq!(market.source, MarketSource::CurvePoll);
        assert!(market.price_sol_per_token() > 0.0);
        // Reserves only — the curve account carries no flow at all.
        assert!(!market.has_trade_flow());
        assert_eq!((market.buys, market.sells), (0, 0));
        assert_eq!(market.volume_in_lamports, 0);
    }

    #[test]
    fn a_poll_is_ignored_for_an_untracked_mint() {
        let mut t = MarketTracker::default();
        assert!(!t.apply_curve_snapshot(mint(), 1_000, 1_000, 1));
        assert!(t.tracked_mints().is_empty());
    }

    #[test]
    fn an_empty_curve_read_does_not_blank_a_working_price() {
        // A failed/zeroed account read must not be mistaken for "the curve is
        // empty" — that would wipe a price the panel is actively showing.
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        t.observe(m, 1_000, 1_000, true, 2_000, 1_000, 100);
        assert!(!t.apply_curve_snapshot(m, 0, 0, 1_000));
        assert_eq!(t.get(&m).unwrap().virtual_sol_reserves, 2_000);
    }

    #[test]
    fn a_live_trade_event_beats_a_poll() {
        // While the trade feed is working it is both more current (shreds land
        // pre-confirmation) and richer, so a poll must not take the market over.
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        t.observe(m, 1_000, 1_000, true, 2_000, 1_000, 1_000);
        assert!(
            !t.apply_curve_snapshot(m, 9_999, 1_000, 1_001),
            "poll overrode a live trade event"
        );
        let market = t.get(&m).unwrap();
        assert_eq!(market.virtual_sol_reserves, 2_000);
        assert_eq!(market.source, MarketSource::TradeEvent);
        assert!(market.has_trade_flow());
    }

    #[test]
    fn a_poll_takes_over_once_the_trade_feed_goes_quiet() {
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        t.observe(m, 1_000, 1_000, true, 2_000, 1_000, 1_000);
        let later = 1_000 + TRADE_EVENT_AUTHORITY_SECS;
        assert!(t.apply_curve_snapshot(m, 9_999, 1_000, later));
        let market = t.get(&m).unwrap();
        assert_eq!(market.virtual_sol_reserves, 9_999);
        assert_eq!(market.source, MarketSource::CurvePoll);
        // The flow figures stay as measured; the poll adds nothing to them.
        assert_eq!((market.buys, market.sells), (1, 0));
    }

    #[test]
    fn a_stale_poll_does_not_clobber_newer_state() {
        // Two refresh ticks can complete out of order (retry, slow endpoint).
        // Reserves are absolute state, so the older read would rewind the price.
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        assert!(t.apply_curve_snapshot(m, 5_000, 1_000, 2_000));
        assert!(!t.apply_curve_snapshot(m, 1_000, 1_000, 1_900));
        let market = t.get(&m).unwrap();
        assert_eq!(market.virtual_sol_reserves, 5_000);
        assert_eq!(market.last_update_unix, 2_000, "clock walked backwards");
    }

    #[test]
    fn a_poll_never_blocks_a_later_trade_event() {
        // The poll stamps a *local* clock; trade events carry a validator one,
        // usually a little behind it. Comparing the two would let the fallback
        // swallow the very events it exists to stand in for.
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        let polled_at = now_unix();
        assert!(t.apply_curve_snapshot(m, 5_000, 1_000, polled_at));
        // Event timestamp behind the poll's wall clock, as in production.
        t.observe(m, 1_000, 1_000, true, 7_000, 1_000, polled_at.saturating_sub(2));
        let market = t.get(&m).unwrap();
        assert_eq!(market.virtual_sol_reserves, 7_000, "trade event dropped");
        assert_eq!(market.source, MarketSource::TradeEvent);
        assert_eq!(market.buys, 1);
    }

    #[test]
    fn age_is_never_negative_when_the_validator_clock_is_ahead() {
        // Leader clock skew puts event timestamps in the future; a raw
        // subtraction renders "last trade -4s ago".
        let mut t = MarketTracker::default();
        let m = mint();
        t.track(m);
        let future = now_unix().saturating_add(3_600);
        t.observe(m, 1_000, 1_000, true, 1_000, 1_000, future);
        assert_eq!(t.get(&m).unwrap().age_secs(), 0);
    }
}
