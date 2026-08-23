//! Bonding-curve quote math, following the formulas documented on
//! `BuyExactSolIn` in the pumpfun decoder.

#[derive(Debug, Clone, Copy)]
pub struct CurveState {
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    pub protocol_fee_bps: u64,
    pub creator_fee_bps: u64,
}

impl CurveState {
    /// Tokens received for `spendable_sol_in` lamports (fees deducted from the
    /// input, per the `BuyExactSolIn` quote formula).
    pub fn tokens_out_for_sol(&self, spendable_sol_in: u64) -> u64 {
        let total_fee_bps = self.protocol_fee_bps + self.creator_fee_bps;
        let mut net_sol = (spendable_sol_in as u128) * 10_000 / (10_000 + total_fee_bps as u128);
        // Each fee rounds up independently, matching the program.
        let fees = (net_sol * self.protocol_fee_bps as u128).div_ceil(10_000)
            + (net_sol * self.creator_fee_bps as u128).div_ceil(10_000);
        if net_sol + fees > spendable_sol_in as u128 {
            net_sol -= net_sol + fees - spendable_sol_in as u128;
        }
        if net_sol <= 1 {
            return 0;
        }
        let tokens_out = (net_sol - 1) * self.virtual_token_reserves as u128
            / (self.virtual_sol_reserves as u128 + net_sol - 1);
        tokens_out as u64
    }

    /// Shift reserves as if `spendable_sol_in` lamports were spent buying —
    /// used to price in the creator's dev buy (or an assumed allowance for it)
    /// before quoting our own buy.
    pub fn after_buy(&self, spendable_sol_in: u64) -> CurveState {
        let tokens_out = self.tokens_out_for_sol(spendable_sol_in);
        let total_fee_bps = self.protocol_fee_bps + self.creator_fee_bps;
        let net_sol = (spendable_sol_in as u128) * 10_000 / (10_000 + total_fee_bps as u128);
        CurveState {
            virtual_sol_reserves: self.virtual_sol_reserves.saturating_add(net_sol as u64),
            virtual_token_reserves: self.virtual_token_reserves.saturating_sub(tokens_out),
            ..*self
        }
    }
}

/// `min_tokens_out` for a buy of `spendable_sol_in`, assuming up to
/// `dev_buy_allowance` lamports were spent on the curve before us (creator dev
/// buy in the create transaction), minus `slippage_bps` tolerance.
pub fn min_tokens_out(
    initial: &CurveState,
    spendable_sol_in: u64,
    dev_buy_allowance: u64,
    slippage_bps: u64,
) -> u64 {
    let state = if dev_buy_allowance > 0 {
        initial.after_buy(dev_buy_allowance)
    } else {
        *initial
    };
    let expected = state.tokens_out_for_sol(spendable_sol_in);
    let floored =
        ((expected as u128) * (10_000u128.saturating_sub(slippage_bps as u128)) / 10_000) as u64;
    // The program rejects a zero minimum with BuyZeroAmount (6020), so a
    // 100%-slippage or dust-sized buy would fail on-chain rather than filling
    // at any price. Clamp to 1 base unit — the intent of "accept any amount" —
    // but leave a genuinely zero-output quote at zero so the caller can skip it.
    if floored == 0 && expected > 0 {
        1
    } else {
        floored
    }
}

/// A price ceiling for a snipe, expressed as `min_tokens_out`.
///
/// This is deliberately NOT a precise quote. Modelling the exact output of a
/// v2 buy requires knowing the dev buy, the fee split and the exact semantics
/// of `BuyExactQuoteInV2` — and the one time this codebase trusted a v2 quote
/// it demanded 5.5x what the curve could pay and every buy died with
/// `BuySlippageBelowMinTokensOut` (6042). The recorded explanation for that
/// (v2 opening reserves differing from v1) is verifiably false: Global's
/// `initial_virtual_token_reserves` is 1,073,000,000,000,000 and a live v2
/// curve reads the same, with both quoting 30 SOL. So the real cause is
/// unknown, and a floor built on a precise quote would be built on sand.
///
/// A ceiling needs none of that. It asks one coarse question: *how much worse
/// than the opening price am I willing to fill at?* At `multiple = 3` the buy
/// reverts if it would land at more than three times the price a buy into an
/// untouched curve would have paid. A normal snipe — even behind a healthy dev
/// buy and a few other snipers — fills comfortably; a buy behind someone who
/// has pushed the curve 10x does not.
///
/// That is exactly the frontrun protection `min_tokens_out = 1` gives up. With
/// no floor a sandwich can take the entire position at any price and the
/// transaction still succeeds; losing the trade is strictly better than losing
/// the money.
///
/// Returns `None` when disabled, so the caller keeps its existing behaviour.
pub fn price_ceiling_min_tokens(
    opening: &CurveState,
    spendable_sol_in: u64,
    max_price_multiple: f64,
) -> Option<u64> {
    if !(max_price_multiple.is_finite() && max_price_multiple >= 1.0) {
        return None;
    }
    let at_open = opening.tokens_out_for_sol(spendable_sol_in);
    if at_open == 0 {
        return None;
    }
    // Paying `multiple` times the opening price means receiving `1/multiple`
    // of the tokens, so the floor is the opening output divided by it.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let floor = ((at_open as f64) / max_price_multiple) as u64;
    // Never return 0: the program rejects a zero minimum with BuyZeroAmount
    // (6020), which would fail the buy outright rather than leave it unguarded.
    Some(floor.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Historical pump.fun launch constants: 30 SOL virtual / 1.073B tokens
    // (6 decimals), 1% protocol fee + 0.05% creator fee.
    fn fresh_curve() -> CurveState {
        CurveState {
            virtual_sol_reserves: 30_000_000_000,
            virtual_token_reserves: 1_073_000_000_000_000,
            protocol_fee_bps: 100,
            creator_fee_bps: 5,
        }
    }

    #[test]
    fn quote_on_fresh_curve() {
        let curve = fresh_curve();
        // 1 SOL in on a fresh curve buys roughly 1/30th of virtual tokens.
        let out = curve.tokens_out_for_sol(1_000_000_000);
        assert!(
            out > 33_000_000_000_000 && out < 36_000_000_000_000,
            "{out}"
        );
        // More SOL always buys more tokens.
        assert!(curve.tokens_out_for_sol(2_000_000_000) > out);
        // Fees reduce output vs a fee-less curve.
        let no_fees = CurveState {
            protocol_fee_bps: 0,
            creator_fee_bps: 0,
            ..curve
        };
        assert!(no_fees.tokens_out_for_sol(1_000_000_000) > out);
    }

    #[test]
    fn dev_buy_shifts_price() {
        let curve = fresh_curve();
        let shifted = curve.after_buy(2_000_000_000);
        assert!(shifted.virtual_sol_reserves > curve.virtual_sol_reserves);
        assert!(shifted.virtual_token_reserves < curve.virtual_token_reserves);
        // Buying after the dev buy yields fewer tokens for the same SOL.
        assert!(
            shifted.tokens_out_for_sol(1_000_000_000) < curve.tokens_out_for_sol(1_000_000_000)
        );
    }

    #[test]
    fn min_tokens_out_applies_allowance_and_slippage() {
        let curve = fresh_curve();
        let plain = curve.tokens_out_for_sol(1_000_000_000);
        let min = min_tokens_out(&curve, 1_000_000_000, 2_000_000_000, 500);
        assert!(min < plain);
        // 100% slippage clamps to 1, not 0: the program rejects a zero
        // minimum with BuyZeroAmount, so "accept any amount" must be 1.
        assert_eq!(min_tokens_out(&curve, 1_000_000_000, 0, 10_000), 1);
        // A genuinely zero-output quote stays zero so callers can skip it.
        assert_eq!(min_tokens_out(&curve, 0, 0, 0), 0);
    }

    #[test]
    fn zero_input_is_zero_output() {
        assert_eq!(fresh_curve().tokens_out_for_sol(0), 0);
    }
}

#[cfg(test)]
mod ceiling_tests {
    use super::*;

    /// Mainnet opening curve, verified against Global and a live v2 curve.
    fn opening() -> CurveState {
        CurveState {
            virtual_sol_reserves: 30_000_000_000,
            virtual_token_reserves: 1_073_000_000_000_000,
            protocol_fee_bps: 95,
            creator_fee_bps: 5,
        }
    }

    #[test]
    fn the_floor_is_the_opening_output_divided_by_the_multiple() {
        let buy = 100_000_000; // 0.1 SOL
        let at_open = opening().tokens_out_for_sol(buy);
        let floor = price_ceiling_min_tokens(&opening(), buy, 3.0).unwrap();
        let expected = at_open / 3;
        assert!(
            floor.abs_diff(expected) <= 1,
            "floor {floor} should be ~{expected}"
        );
        // And it must be well below the opening output, or a normal snipe
        // behind a dev buy would revert.
        assert!(floor < at_open);
    }

    #[test]
    fn a_multiple_of_one_demands_the_full_opening_price() {
        let buy = 100_000_000;
        let at_open = opening().tokens_out_for_sol(buy);
        let floor = price_ceiling_min_tokens(&opening(), buy, 1.0).unwrap();
        assert!(floor.abs_diff(at_open) <= 1);
    }

    #[test]
    fn nonsense_multiples_disable_the_ceiling_rather_than_guessing() {
        // Below 1.0 would demand MORE tokens than an untouched curve can pay,
        // which fails every buy — the 6042 failure mode. Refuse instead.
        for m in [0.0, 0.5, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                price_ceiling_min_tokens(&opening(), 100_000_000, m).is_none(),
                "multiple {m} should disable, not produce a floor"
            );
        }
    }

    #[test]
    fn the_floor_is_never_zero() {
        // 0 would be rejected on-chain as BuyZeroAmount (6020), failing the buy
        // outright rather than leaving it unguarded. The multiple has to exceed
        // the opening output for the division to floor to zero at all — 1,000
        // lamports still buys ~35M base units, so a merely large multiple is
        // not enough to exercise this.
        let buy = 1_000;
        let at_open = opening().tokens_out_for_sol(buy);
        let floor = price_ceiling_min_tokens(&opening(), buy, (at_open as f64) * 2.0);
        assert_eq!(floor, Some(1), "at_open was {at_open}");
    }

    #[test]
    fn a_pushed_curve_falls_below_the_floor() {
        // Someone front-runs with 200 SOL; our 0.1 SOL now buys far less.
        let buy = 100_000_000;
        let floor = price_ceiling_min_tokens(&opening(), buy, 3.0).unwrap();
        let pushed = opening().after_buy(200_000_000_000);
        let actual = pushed.tokens_out_for_sol(buy);
        assert!(
            actual < floor,
            "a 200 SOL frontrun should breach a 3x ceiling: got {actual}, floor {floor}"
        );
    }

    #[test]
    fn an_ordinary_dev_buy_still_fills() {
        // The protection is worthless if it rejects normal launches. A 2 SOL
        // dev buy ahead of us must stay comfortably above a 3x ceiling.
        let buy = 100_000_000;
        let floor = price_ceiling_min_tokens(&opening(), buy, 3.0).unwrap();
        let after_dev = opening().after_buy(2_000_000_000);
        assert!(
            after_dev.tokens_out_for_sol(buy) > floor,
            "a 2 SOL dev buy must not trip a 3x ceiling"
        );
    }
}
