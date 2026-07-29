use {
    crate::{
        config::{Config, SendMode},
        processor::SnipeSignal,
        pump::{
            instructions::{self, CoinAccounts, StaticAccounts},
            quote::{self, CurveState},
        },
        sender::{jito, rpc},
    },
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_hash::Hash,
    solana_instruction::Instruction,
    solana_keypair::Keypair,
    solana_message::{v0, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::sync::Arc,
    tokio::sync::{mpsc, RwLock},
};

pub struct BuyDispatcher {
    cfg: Arc<Config>,
    rpc: Arc<RpcClient>,
    blockhash: Arc<RwLock<Hash>>,
    statics: StaticAccounts,
    initial_curve: CurveState,
}

impl BuyDispatcher {
    pub fn new(
        cfg: Arc<Config>,
        rpc: Arc<RpcClient>,
        blockhash: Arc<RwLock<Hash>>,
        statics: StaticAccounts,
        initial_curve: CurveState,
    ) -> Self {
        Self {
            cfg,
            rpc,
            blockhash,
            statics,
            initial_curve,
        }
    }

    pub async fn run(self, mut signals: mpsc::Receiver<SnipeSignal>) {
        while let Some(signal) = signals.recv().await {
            if let Err(err) = self.dispatch(&signal).await {
                log::error!("[{}] dispatch failed: {err}", signal.mint);
            }
        }
    }

    async fn dispatch(&self, signal: &SnipeSignal) -> Result<(), String> {
        let coin = CoinAccounts::new(
            signal.mint,
            signal.bonding_curve,
            signal.associated_bonding_curve,
            &signal.creator,
        );
        let min_tokens_out = quote::min_tokens_out(
            &self.initial_curve,
            self.cfg.buy_amount_lamports,
            signal.dev_buy_lamports,
            self.cfg.slippage_bps,
        );
        let blockhash = *self.blockhash.read().await;

        let mut txs = Vec::with_capacity(self.cfg.buyers.len());
        for (i, buyer) in self.cfg.buyers.iter().enumerate() {
            // In jito mode the tip rides on the last buyer's transaction.
            let tip = (self.cfg.send_mode == SendMode::JitoBundle
                && i == self.cfg.buyers.len() - 1)
                .then_some(self.cfg.jito_tip_lamports);
            txs.push(self.build_buy_tx(buyer, &coin, min_tokens_out, blockhash, tip)?);
        }

        log::info!(
            "[{}] {} ({}): dispatching {} buys of {} lamports each (min_tokens_out={}, dev_buy={})",
            signal.mint,
            signal.name,
            signal.symbol,
            txs.len(),
            self.cfg.buy_amount_lamports,
            min_tokens_out,
            signal.dev_buy_lamports,
        );

        match self.cfg.send_mode {
            SendMode::Simulate => rpc::simulate_all(&self.rpc, &txs).await,
            SendMode::RpcSpray => rpc::spray(&self.rpc, &txs).await,
            SendMode::JitoBundle => jito::send_bundle(&self.cfg.jito_block_engine_url, &txs).await,
        }
    }

    fn build_buy_tx(
        &self,
        buyer: &Keypair,
        coin: &CoinAccounts,
        min_tokens_out: u64,
        blockhash: Hash,
        jito_tip_lamports: Option<u64>,
    ) -> Result<VersionedTransaction, String> {
        let buyer_pk = buyer.pubkey();
        let mut ixs: Vec<Instruction> = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(self.cfg.compute_unit_limit),
            ComputeBudgetInstruction::set_compute_unit_price(self.cfg.priority_fee_micro_lamports),
            instructions::create_ata_idempotent(&buyer_pk, &buyer_pk, &coin.mint),
            instructions::buy_exact_sol_in(
                &self.statics,
                coin,
                &buyer_pk,
                self.cfg.buy_amount_lamports,
                min_tokens_out,
                self.cfg.track_volume,
            ),
        ];
        if let Some(tip) = jito_tip_lamports {
            ixs.push(jito::tip_instruction(&buyer_pk, tip));
        }

        let message = v0::Message::try_compile(&buyer_pk, &ixs, &[], blockhash)
            .map_err(|e| format!("compile message: {e}"))?;
        VersionedTransaction::try_new(VersionedMessage::V0(message), &[buyer])
            .map_err(|e| format!("sign tx: {e}"))
    }
}
