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
        let fees = div_ceil(net_sol * self.protocol_fee_bps as u128, 10_000)
            + div_ceil(net_sol * self.creator_fee_bps as u128, 10_000);
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

fn div_ceil(a: u128, b: u128) -> u128 {
    (a + b - 1) / b
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
