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

/// Everything the dispatcher needs to fire buys for a freshly created coin.
#[derive(Debug, Clone)]
pub struct SnipeSignal {
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub bonding_curve: Pubkey,
    pub associated_bonding_curve: Pubkey,
    /// Upper bound on what the creator spent buying in the create transaction
    /// (from the dev-buy instruction in the same tx), used to shift the quote.
    pub dev_buy_lamports: u64,
    pub name: String,
    pub symbol: String,
    pub create_signature: String,
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

impl Processor<InstructionProcessorInputType<'_, PumpfunInstruction>> for SniperProcessor {
    async fn process(
        &mut self,
        input: &InstructionProcessorInputType<'_, PumpfunInstruction>,
    ) -> CarbonResult<()> {
        let (create, accounts) = match input.decoded_instruction {
            PumpfunInstruction::Create { data, accounts, .. } => (data, accounts),
            PumpfunInstruction::CreateV2 { data, .. } => {
                if self.cfg.watched_creators.contains(&data.creator) {
                    log::warn!(
                        "watched creator {} launched via create_v2 (quote-mint flow) — v2 buys not implemented yet, skipping",
                        data.creator
                    );
                }
                return Ok(());
            }
            _ => return Ok(()),
        };

        let tx = &input.metadata.transaction_metadata;
        let creator = create.creator;
        let user = accounts.user;

        if !(self.cfg.watched_creators.contains(&creator)
            || self.cfg.watched_creators.contains(&user)
            || self.cfg.watched_creators.contains(&tx.fee_payer))
        {
            return Ok(());
        }
        if self.cfg.blacklisted_creators.contains(&creator)
            || self.cfg.blacklisted_creators.contains(&user)
        {
            log::info!("skipping {}: creator blacklisted", accounts.mint);
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
                accounts.mint,
                pre
            );
            return Ok(());
        }
        let spent = pre - post;
        if spent > self.cfg.max_creator_buy_lamports {
            log::info!(
                "skipping {}: creator spent {} lamports in create tx, above maximum",
                accounts.mint,
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
                log::info!("skipping {}: create is {}ms old", accounts.mint, age_ms);
                return Ok(());
            }
        }

        if self.sniped_mints.contains(&accounts.mint) {
            return Ok(());
        }
        if self.sniped_mints.len() >= self.cfg.max_positions {
            log::warn!(
                "skipping {}: max positions ({}) reached",
                accounts.mint,
                self.cfg.max_positions
            );
            return Ok(());
        }
        self.sniped_mints.insert(accounts.mint);

        let signal = SnipeSignal {
            mint: accounts.mint,
            creator,
            bonding_curve: accounts.bonding_curve,
            associated_bonding_curve: accounts.associated_bonding_curve,
            dev_buy_lamports: dev_buy_lamports(tx).unwrap_or(self.cfg.max_creator_buy_lamports),
            name: create.name.clone(),
            symbol: create.symbol.clone(),
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
        let lamports = if disc == BUY_DISCRIMINATOR {
            u64::from_le_bytes(ix.data[16..24].try_into().ok()?)
        } else if disc == BUY_EXACT_SOL_IN_DISCRIMINATOR {
            u64::from_le_bytes(ix.data[8..16].try_into().ok()?)
        } else {
            continue;
        };
        return Some(lamports);
    }
    None
}
