use {
    crate::config::Config, solana_client::nonblocking::rpc_client::RpcClient, solana_signer::Signer,
};

/// Batch-check every buyer wallet's balance before going live. A wallet that
/// cannot cover its buy plus fees and rent is a dead transaction at snipe
/// time, so surface it now rather than in the middle of a launch.
///
/// Returns the indices of wallets that are underfunded.
pub async fn preflight_balances(cfg: &Config, rpc: &RpcClient) -> Result<Vec<usize>, String> {
    let pubkeys: Vec<_> = cfg.buyers.iter().map(|b| b.keypair.pubkey()).collect();

    let mut balances = Vec::with_capacity(pubkeys.len());
    // get_multiple_accounts caps at 100 keys per call.
    for chunk in pubkeys.chunks(100) {
        let accounts = rpc
            .get_multiple_accounts(chunk)
            .await
            .map_err(|e| format!("balance preflight failed: {e}"))?;
        balances.extend(
            accounts
                .into_iter()
                .map(|account| account.map(|a| a.lamports).unwrap_or_default()),
        );
    }

    // In Balance mode the buy size is derived here, because this is the first
    // point at which live balances exist. Doing it before the underfunded
    // check matters: the check must test the amount that will actually be
    // spent, not the placeholder the Config was built with.
    if cfg.buy_sizing == crate::config::BuySizing::Balance {
        size_buys_from_balances(cfg, &balances);
    }

    let mut underfunded = Vec::new();
    let mut total = 0u64;
    for (i, buyer) in cfg.buyers.iter().enumerate() {
        // `balances` is built from the same pubkey list in the same order, but
        // index it defensively: a panic here happens before the pipeline starts
        // and takes the whole sniper down with a backtrace instead of a reason.
        let Some(&balance) = balances.get(i) else {
            log::warn!("wallet #{i}: no balance returned by RPC — treating as underfunded");
            underfunded.push(i);
            continue;
        };
        let amount = buyer.buy_amount_lamports();
        total = total.saturating_add(balance);

        // In Balance mode the amount was DERIVED from this balance minus the
        // fee reserve, so it is affordable by construction and demanding the
        // funding buffer on top double-counts the same money twice. Worse, the
        // balance cancels out of `balance < amount + buffer` entirely, leaving
        // a check whose outcome depends only on whether `gas_reserve` happens
        // to exceed `funding_buffer_lamports` — two unrelated knobs. With the
        // shipped .env.example that flags a wallet holding 10 SOL as
        // underfunded and aborts the run.
        //
        // What actually matters in this mode is whether the wallet could
        // afford anything at all, which `size_buys_from_balances` already
        // decided by setting the amount to 0.
        let underfunded_here = match cfg.buy_sizing {
            crate::config::BuySizing::Balance => amount == 0,
            crate::config::BuySizing::Fixed => {
                balance < amount.saturating_add(cfg.funding_buffer_lamports)
            }
        };
        if underfunded_here {
            log::warn!(
                "wallet #{i} {} underfunded: has {} lamports, needs {}",
                buyer.keypair.pubkey(),
                balance,
                match cfg.buy_sizing {
                    crate::config::BuySizing::Balance =>
                        crate::dispatch::gas_reserve(cfg, buyer),
                    crate::config::BuySizing::Fixed =>
                        amount.saturating_add(cfg.funding_buffer_lamports),
                }
            );
            underfunded.push(i);
        }
    }

    log::info!(
        "wallet preflight: {} wallets, {} lamports total, {} underfunded",
        cfg.buyers.len(),
        total,
        underfunded.len()
    );
    Ok(underfunded)
}

/// Re-derive every wallet's buy size from live balances.
///
/// After a snipe the wallets hold roughly only their fee reserve, but the
/// amounts were computed once at startup — so without this the next launch
/// builds thirty transactions that try to spend SOL that is already gone.
/// Every one fails on chain, and because nothing landed the retry loop rebuilds
/// the same impossible amounts and pays the fees again. `MAX_POSITIONS`
/// defaults to 5, so that repeats on up to four more launches.
///
/// Runs on a timer off the hot path — never during a dispatch — so a snipe is
/// never delayed by an RPC round trip. Wallets that fall below their reserve
/// are sized to zero and skipped by the dispatcher until they are re-funded,
/// at which point this picks them back up automatically.
pub async fn refresh_buy_sizes(cfg: &Config, rpc: &RpcClient) {
    if cfg.buy_sizing != crate::config::BuySizing::Balance {
        return;
    }
    let pubkeys: Vec<_> = cfg.buyers.iter().map(|b| b.keypair.pubkey()).collect();
    let mut balances = Vec::with_capacity(pubkeys.len());
    for chunk in pubkeys.chunks(100) {
        match rpc.get_multiple_accounts(chunk).await {
            Ok(accounts) => balances.extend(
                accounts
                    .into_iter()
                    .map(|a| a.map(|a| a.lamports).unwrap_or_default()),
            ),
            // A failed refresh must leave the previous sizes alone rather than
            // zero the fleet: a transient RPC error is not evidence that the
            // wallets are empty.
            Err(err) => {
                log::warn!("buy-size refresh skipped: {err}");
                return;
            }
        }
    }
    size_buys_from_balances(cfg, &balances);
}

/// Set each wallet's buy to a random slice of its own spendable balance.
///
/// This is what makes thirty buys look like thirty people. The wallets were
/// funded at jittered amounts, at random times, so their balances already
/// differ; spending nearly all of each one inherits that variance instead of
/// inventing a distribution. And a wallet that apes very nearly everything it
/// holds is what an organic retail buyer looks like — whereas thirty
/// byte-identical buys in one block is the literal definition of the cluster
/// every terminal scans for.
///
/// `spendable` subtracts the same fee reserve the manual buy path uses, so a
/// wallet never builds a buy it cannot pay for. A wallet too thin to cover its
/// own reserve is set to zero and picked up by the underfunded check.
fn size_buys_from_balances(cfg: &Config, balances: &[u64]) {
    let (lo, hi) = (cfg.buy_balance_pct_min, cfg.buy_balance_pct_max);
    for (i, buyer) in cfg.buyers.iter().enumerate() {
        let Some(&balance) = balances.get(i) else {
            continue;
        };
        let reserve = crate::dispatch::gas_reserve(cfg, buyer);
        let spendable = balance.saturating_sub(reserve);
        if spendable == 0 {
            buyer.set_buy_amount_lamports(0);
            continue;
        }
        // Uniform in [lo, hi] percent. Falls back to the low end rather than
        // the high end if entropy is unavailable: underspending is recoverable,
        // overspending against a stale reserve is a failed buy.
        let span = hi.saturating_sub(lo).saturating_add(1);
        let pct = match os_random_u64() {
            Some(r) => lo.saturating_add(r % span),
            None => {
                // The whole point of this mode is that the percentages differ.
                // Falling back silently would leave the fleet buying one
                // percentage of their balances and reporting success.
                log::warn!(
                    "/dev/urandom unreadable — wallet #{i} falls back to {lo}%. If this repeats \
                     for every wallet the per-wallet variance is gone."
                );
                lo
            }
        };
        let amount = spendable.saturating_mul(pct).saturating_div(100);
        buyer.set_buy_amount_lamports(amount);
        log::debug!(
            "wallet #{i} {}: balance {} - reserve {} = {} spendable, buying {pct}% = {amount}",
            buyer.keypair.pubkey(),
            balance,
            reserve,
            spendable,
        );
    }
}

/// Eight bytes of OS entropy, or `None`.
fn os_random_u64() -> Option<u64> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut b = [0u8; 8];
    f.read_exact(&mut b).ok()?;
    Some(u64::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    /// The bug this pins, in isolated arithmetic.
    ///
    /// In Balance mode the buy is `balance - reserve`, and the old preflight
    /// then demanded `buy + funding_buffer`. Expand it:
    ///
    ///   flagged  <=>  balance < (balance - reserve) + buffer
    ///            <=>  0 < buffer - reserve
    ///            <=>  reserve < buffer
    ///
    /// The wallet's balance cancels out entirely, so a wallet holding 10 SOL is
    /// flagged identically to one holding 0.02 — and whether the run starts at
    /// all depends only on an accidental relationship between two unrelated
    /// knobs. Under the shipped .env.example it flagged every wallet.
    #[test]
    fn balance_mode_must_not_stack_the_funding_buffer_on_the_reserve() {
        fn old_check(balance: u64, reserve: u64, buffer: u64) -> bool {
            let buy = balance.saturating_sub(reserve); // pct = 100
            balance < buy.saturating_add(buffer)
        }
        let (reserve, buffer) = (9_125_000u64, 10_000_000u64); // .env.example
        for balance in [50_000_000u64, 150_000_000, 10_000_000_000] {
            assert!(
                old_check(balance, reserve, buffer),
                "the old check should have flagged {balance}, confirming the bug's reach"
            );
        }
        // A well-funded wallet is affordable by construction: the amount was
        // derived from its own balance minus its own reserve.
        for balance in [50_000_000u64, 150_000_000, 10_000_000_000] {
            let buy = balance.saturating_sub(reserve);
            assert!(buy > 0 && buy <= balance, "balance {balance} must afford {buy}");
        }
    }

    /// A wallet below its own reserve is sized to zero, and zero must be the
    /// signal that keeps it off the wire — a 0-lamport buy is still signed,
    /// still pays priority fees and tips, still creates the ATAs, and is then
    /// rejected by the program with BuyZeroAmount.
    #[test]
    fn a_wallet_below_its_reserve_sizes_to_zero() {
        let reserve = 17_000_000u64;
        for (balance, expect_zero) in [(0u64, true), (12_000_000, true), (17_000_000, true), (50_000_000, false)] {
            let spendable = balance.saturating_sub(reserve);
            assert_eq!(spendable == 0, expect_zero, "balance {balance}");
        }
    }
}
