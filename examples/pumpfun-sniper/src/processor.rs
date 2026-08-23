use {
    crate::{config::Config, market::MarketTracker},
    carbon_core::{
        error::CarbonResult, instruction::InstructionProcessorInputType, processor::Processor,
    },
    carbon_pumpfun_decoder::instructions::PumpfunInstruction,
    solana_pubkey::Pubkey,
    std::{
        collections::HashSet,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    },
    tokio::sync::{mpsc, RwLock},
};

/// Which pump launch flow created the coin, and therefore which buy
/// instruction can fill it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// `create` — classic SPL Token coin, bought with `buy_exact_sol_in`
    /// spending native lamports.
    V1,
    /// `create_v2` — Token-2022 coin trading against a quote mint (WSOL today),
    /// bought with `buy_exact_quote_in_v2`. `mayhem` mirrors the create
    /// instruction's `is_mayhem_mode` and decides which program
    /// `bonding_curve_v2` derives on.
    V2 { mayhem: bool },
}

impl Launch {
    pub fn is_v2(&self) -> bool {
        matches!(self, Launch::V2 { .. })
    }
}

/// Everything the dispatcher needs to fire buys for a freshly created coin.
#[derive(Debug, Clone)]
pub struct SnipeSignal {
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub bonding_curve: Pubkey,
    pub associated_bonding_curve: Pubkey,
    pub launch: Launch,
    /// Upper bound on what the creator spent buying in the create transaction
    /// (from the dev-buy instruction in the same tx), used to shift the quote.
    pub dev_buy_lamports: u64,
    pub name: String,
    pub symbol: String,
    pub create_signature: String,
    /// Slot the create landed in. The only exact basis for judging whether a
    /// buy achieved block 0 (same slot), block 1, or worse — wall-clock
    /// timestamps have 1s granularity against ~400ms slots.
    pub create_slot: u64,
}

/// The fields both `create` and `create_v2` provide, normalised so the guards
/// below run once instead of per launch flow.
struct Launched {
    launch: Launch,
    mint: Pubkey,
    bonding_curve: Pubkey,
    associated_bonding_curve: Pubkey,
    user: Pubkey,
    creator: Pubkey,
    name: String,
    symbol: String,
}

pub struct SniperProcessor {
    cfg: Arc<Config>,
    /// Shared with the console so `watch` / `unwatch` take effect on the live
    /// pipeline without a restart. Seeded from `WATCHED_CREATORS`.
    watched: Arc<RwLock<HashSet<Pubkey>>>,
    /// Live price/volume, folded from pump `TradeEvent`s. Shared with the
    /// console so `status` reports chain-published state rather than a model.
    market: Arc<RwLock<MarketTracker>>,
    signals: mpsc::Sender<SnipeSignal>,
    sniped_mints: HashSet<Pubkey>,
}

impl SniperProcessor {
    pub fn new(
        cfg: Arc<Config>,
        watched: Arc<RwLock<HashSet<Pubkey>>>,
        market: Arc<RwLock<MarketTracker>>,
        signals: mpsc::Sender<SnipeSignal>,
    ) -> Self {
        Self {
            cfg,
            watched,
            market,
            signals,
            sniped_mints: HashSet::new(),
        }
    }
}

/// Anchor discriminators of the buy variants a creator's dev buy can use.
const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const BUY_EXACT_SOL_IN_DISCRIMINATOR: [u8; 8] = [56, 252, 116, 8, 158, 223, 205, 95];
/// v2 dev buys: `buy_v2` takes (amount, max_sol_cost) like `buy`, and
/// `buy_exact_quote_in_v2` takes (spendable_quote_in, min_tokens_out) like
/// `buy_exact_sol_in`, so the same two offsets apply.
const BUY_V2_DISCRIMINATOR: [u8; 8] = [184, 23, 238, 97, 103, 197, 211, 61];
const BUY_EXACT_QUOTE_IN_V2_DISCRIMINATOR: [u8; 8] = [194, 171, 28, 70, 104, 77, 91, 47];

impl Processor<InstructionProcessorInputType<'_, PumpfunInstruction>> for SniperProcessor {
    async fn process(
        &mut self,
        input: &InstructionProcessorInputType<'_, PumpfunInstruction>,
    ) -> CarbonResult<()> {
        // Both launch flows carry the same coin identity; only the buy path
        // downstream differs. `create_v2` additionally reports mayhem mode.
        // Every pump trade emits a TradeEvent carrying the filled amounts and
        // the curve's virtual reserves afterwards. Folding those in gives live
        // price and volume without modelling the bonding curve — the model we
        // know is wrong for v2.
        if let PumpfunInstruction::CpiEvent { data, .. } = input.decoded_instruction {
            if let carbon_pumpfun_decoder::instructions::cpi_event::CpiEvent::TradeEvent(t) = data {
                // This fires for EVERY pump trade on the network — on
                // shredstream, where nothing is filtered server-side, that is
                // the full network trade rate. Membership is a read, so test it
                // under `read()` and take `write()` only on the handful of
                // mints we actually hold. Taking the write lock first put a
                // global exclusive lock on the detection path, serialised
                // against the 1 Hz panel redraw.
                let tracked = { self.market.read().await.is_tracked(&t.mint) };
                if tracked {
                    self.market.write().await.observe(
                        t.mint,
                        t.sol_amount,
                        t.token_amount,
                        t.is_buy,
                        t.virtual_sol_reserves,
                        t.virtual_token_reserves,
                        t.timestamp,
                    );
                }
            }
            return Ok(());
        }

        let coin = match input.decoded_instruction {
            PumpfunInstruction::Create { data, accounts, .. } => Launched {
                launch: Launch::V1,
                mint: accounts.mint,
                bonding_curve: accounts.bonding_curve,
                associated_bonding_curve: accounts.associated_bonding_curve,
                user: accounts.user,
                creator: data.creator,
                name: data.name.clone(),
                symbol: data.symbol.clone(),
            },
            PumpfunInstruction::CreateV2 { data, accounts, .. } => Launched {
                launch: Launch::V2 {
                    mayhem: data.is_mayhem_mode,
                },
                mint: accounts.mint,
                bonding_curve: accounts.bonding_curve,
                associated_bonding_curve: accounts.associated_bonding_curve,
                user: accounts.user,
                creator: data.creator,
                name: data.name.clone(),
                symbol: data.symbol.clone(),
            },
            _ => return Ok(()),
        };
        if coin.launch.is_v2() && !self.cfg.snipe_v2 {
            if self.watched.read().await.contains(&coin.creator) {
                log::warn!(
                    "watched creator {} launched {} via create_v2 — SNIPE_V2 is off, skipping",
                    coin.creator,
                    coin.mint
                );
            }
            return Ok(());
        }

        let tx = &input.metadata.transaction_metadata;
        let creator = coin.creator;
        let user = coin.user;

        // Diagnostic: the watched-creator check below returns silently, so a
        // launch that is seen but not matched is indistinguishable from one
        // that never arrived. LOG_ALL_CREATES=true logs every decoded launch
        // with all three identity fields the filter tests, which tells those
        // two cases apart.
        let is_watched = {
            let watched = self.watched.read().await;
            watched.contains(&creator)
                || watched.contains(&user)
                || watched.contains(&tx.fee_payer)
        };
        if self.cfg.log_all_creates {
            log::info!(
                "create seen: {:?} mint={} creator={} user={} fee_payer={} watched={}",
                coin.launch,
                coin.mint,
                creator,
                user,
                tx.fee_payer,
                is_watched
            );
        }

        if !is_watched {
            return Ok(());
        }
        if self.cfg.blacklisted_creators.contains(&creator)
            || self.cfg.blacklisted_creators.contains(&user)
        {
            log::info!("skipping {}: creator blacklisted", coin.mint);
            return Ok(());
        }

        // Balance and freshness guards read meta.pre_balances /
        // meta.post_balances / block_time. A shredstream datasource fabricates
        // all three (shreds exist before execution), so on that feed these are
        // skipped outright rather than evaluated against zeroes. They do not
        // all fail the same way, which is why skipping beats evaluating:
        //
        // - MIN_CREATOR_BALANCE_SOL fails **closed**. pre reads 0, and
        //   `0 < minimum` is TRUE for any minimum > 0, so every launch is
        //   rejected and the sniper silently never fires. Config::from_env
        //   refuses that combination outright.
        // - MAX_CREATOR_BUY_SOL fails **open**. pre and post both read 0, so
        //   `spent` is 0 and never exceeds the ceiling — every launch passes a
        //   guard the operator believes is filtering. Warned about at startup.
        // - MAX_TX_AGE_MS fails **open** too: block_time is the local receive
        //   time, so every create measures ~0ms old.
        if !self.cfg.synthetic_meta {
            // Balance guards on the fee payer (index 0), as the legacy bot did:
            // creators below MIN balance or dev-buying above MAX are skipped.
            let pre = tx.meta.pre_balances.first().copied().unwrap_or_default();
            let post = tx.meta.post_balances.first().copied().unwrap_or_default();
            if post > pre {
                return Ok(());
            }
            if below_balance_floor(pre, self.cfg.min_creator_balance_lamports) {
                log::info!(
                    "skipping {}: creator balance {} below minimum",
                    coin.mint,
                    pre
                );
                return Ok(());
            }
            let spent = pre.saturating_sub(post);
            if above_buy_ceiling(spent, self.cfg.max_creator_buy_lamports) {
                log::info!(
                    "skipping {}: creator spent {} lamports in create tx, above maximum",
                    coin.mint,
                    spent
                );
                return Ok(());
            }

            // Freshness gate: a create observed too long after block time is a
            // stale replay, not a snipe opportunity.
            if let Some(block_time) = tx.block_time {
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or_default();
                let age_ms = now_ms.saturating_sub(block_time.saturating_mul(1_000));
                if age_ms > self.cfg.max_tx_age_ms {
                    log::info!("skipping {}: create is {}ms old", coin.mint, age_ms);
                    return Ok(());
                }
            }
        }

        if self.sniped_mints.contains(&coin.mint) {
            return Ok(());
        }
        // Counts mints sniped for the life of the PROCESS, not positions still
        // open — so after `MAX_POSITIONS` launches the sniper goes permanently
        // inert even if every one of them has been fully sold, with a single
        // warn line as the only sign. Surfaced loudly here until it counts open
        // positions; a silent stop is the failure mode that costs the most.
        if self.sniped_mints.len() >= self.cfg.max_positions {
            log::error!(
                "SNIPER INERT: {} of {} lifetime position slots used. It will not buy again \
                 until restarted, even if every position has been sold.",
                self.sniped_mints.len(),
                self.cfg.max_positions
            );
        }
        if self.sniped_mints.len() >= self.cfg.max_positions {
            log::warn!(
                "skipping {}: max positions ({}) reached",
                coin.mint,
                self.cfg.max_positions
            );
            return Ok(());
        }
        self.sniped_mints.insert(coin.mint);
        // From here on this mint's trades feed the market tracker.
        self.market.write().await.track(coin.mint);

        let signal = SnipeSignal {
            mint: coin.mint,
            creator,
            bonding_curve: coin.bonding_curve,
            associated_bonding_curve: coin.associated_bonding_curve,
            launch: coin.launch,
            dev_buy_lamports: dev_buy_lamports(tx).unwrap_or(self.cfg.max_creator_buy_lamports),
            name: coin.name.clone(),
            symbol: coin.symbol.clone(),
            create_signature: tx.signature.to_string(),
            create_slot: tx.slot,
        };

        // create_slot is the block the create landed in. Comparing it against
        // the slot our buy lands in is the only exact measure of whether we
        // achieved block 0 (same slot), block 1, or worse — wall-clock
        // timestamps have 1s granularity against ~400ms slots and cannot
        // distinguish them.
        log::info!(
            "SNIPE {} ({}) mint={} creator={} create_slot={} sig={}",
            signal.name,
            signal.symbol,
            signal.mint,
            signal.creator,
            tx.slot,
            signal.create_signature
        );

        // try_send: never let a full dispatcher queue stall the geyser pipeline.
        if let Err(err) = self.signals.try_send(signal) {
            log::error!("failed to queue snipe signal: {err}");
        }

        Ok(())
    }
}

/// The MIN_CREATOR_BALANCE_SOL guard: skip a launch whose creator holds less
/// than the floor. Named so its direction can be pinned by a test — on a
/// synthetic-meta feed `pre` is 0 and this returns true, i.e. it fails
/// **closed**, rejecting everything.
fn below_balance_floor(pre_balance_lamports: u64, minimum_lamports: u64) -> bool {
    pre_balance_lamports < minimum_lamports
}

/// The MAX_CREATOR_BUY_SOL guard: skip a launch whose creator dev-bought above
/// the ceiling. On a synthetic-meta feed `spent` is 0 and this returns false,
/// i.e. it fails **open**, admitting everything.
fn above_buy_ceiling(spent_lamports: u64, maximum_lamports: u64) -> bool {
    spent_lamports > maximum_lamports
}

/// Scan the create transaction for the creator's dev-buy instruction and
/// return an upper bound on the SOL it spent on the curve.
fn dev_buy_lamports(tx: &carbon_core::transaction::TransactionMetadata) -> Option<u64> {
    let keys = tx.message.static_account_keys();
    for ix in tx.message.instructions() {
        if keys.get(ix.program_id_index as usize) != Some(&carbon_pumpfun_decoder::PROGRAM_ID) {
            continue;
        }
        if ix.data.len() < 24 {
            continue;
        }
        let disc: [u8; 8] = ix.data[0..8].try_into().ok()?;
        // Buy: (amount, max_sol_cost) — max_sol_cost bounds the dev spend.
        // BuyExactSolIn: (spendable_sol_in, min_tokens_out) — exact spend.
        let lamports = if disc == BUY_DISCRIMINATOR || disc == BUY_V2_DISCRIMINATOR {
            u64::from_le_bytes(ix.data[16..24].try_into().ok()?)
        } else if disc == BUY_EXACT_SOL_IN_DISCRIMINATOR
            || disc == BUY_EXACT_QUOTE_IN_V2_DISCRIMINATOR
        {
            u64::from_le_bytes(ix.data[8..16].try_into().ok()?)
        } else {
            continue;
        };
        return Some(lamports);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two balance guards fail in OPPOSITE directions on a feed that
    /// carries no balances, and the comments here used to claim otherwise. Both
    /// are pinned so the claim cannot rot again.
    #[test]
    fn the_balance_floor_fails_closed_on_zero_balances() {
        // Synthetic meta: pre reads 0. `0 < minimum` is TRUE, so the guard
        // rejects EVERY launch — the sniper silently never fires. It does NOT
        // admit everything.
        assert!(below_balance_floor(0, 1));
        assert!(below_balance_floor(0, 5_000_000_000));
        // A zero floor is "no floor" and admits everything, on any feed.
        assert!(!below_balance_floor(0, 0));
        // Real-metadata behaviour, for contrast.
        assert!(below_balance_floor(1_000, 2_000));
        assert!(!below_balance_floor(2_000, 2_000));
        assert!(!below_balance_floor(3_000, 2_000));
    }

    #[test]
    fn the_dev_buy_ceiling_fails_open_on_zero_balances() {
        // Synthetic meta: pre and post both read 0, so `spent` is 0 and never
        // exceeds any ceiling. THIS is the guard that admits every launch.
        assert!(!above_buy_ceiling(0, 1));
        assert!(!above_buy_ceiling(0, 5_000_000_000));
        // Real-metadata behaviour, for contrast.
        assert!(above_buy_ceiling(3_000, 2_000));
        assert!(!above_buy_ceiling(2_000, 2_000));
    }
}
