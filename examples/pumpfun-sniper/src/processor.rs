use {
    crate::config::Config,
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
    tokio::sync::mpsc,
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
    signals: mpsc::Sender<SnipeSignal>,
    sniped_mints: HashSet<Pubkey>,
}

impl SniperProcessor {
    pub fn new(cfg: Arc<Config>, signals: mpsc::Sender<SnipeSignal>) -> Self {
        Self {
            cfg,
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
            if self.cfg.watched_creators.contains(&coin.creator) {
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

        if !(self.cfg.watched_creators.contains(&creator)
            || self.cfg.watched_creators.contains(&user)
            || self.cfg.watched_creators.contains(&tx.fee_payer))
        {
            return Ok(());
        }
        if self.cfg.blacklisted_creators.contains(&creator)
            || self.cfg.blacklisted_creators.contains(&user)
        {
            log::info!("skipping {}: creator blacklisted", coin.mint);
            return Ok(());
        }

        // Balance guards on the fee payer (index 0), as the legacy bot did:
        // creators below MIN balance or dev-buying above MAX are skipped.
        let pre = tx.meta.pre_balances.first().copied().unwrap_or_default();
        let post = tx.meta.post_balances.first().copied().unwrap_or_default();
        if post > pre {
            return Ok(());
        }
        if pre < self.cfg.min_creator_balance_lamports {
            log::info!(
                "skipping {}: creator balance {} below minimum",
                coin.mint,
                pre
            );
            return Ok(());
        }
        let spent = pre - post;
        if spent > self.cfg.max_creator_buy_lamports {
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
            let age_ms = now_ms - block_time * 1_000;
            if age_ms > self.cfg.max_tx_age_ms {
                log::info!("skipping {}: create is {}ms old", coin.mint, age_ms);
                return Ok(());
            }
        }

        if self.sniped_mints.contains(&coin.mint) {
            return Ok(());
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
        };

        log::info!(
            "SNIPE {} ({}) mint={} creator={} sig={}",
            signal.name,
            signal.symbol,
            signal.mint,
            signal.creator,
            signal.create_signature
        );

        // try_send: never let a full dispatcher queue stall the geyser pipeline.
        if let Err(err) = self.signals.try_send(signal) {
            log::error!("failed to queue snipe signal: {err}");
        }

        Ok(())
    }
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
