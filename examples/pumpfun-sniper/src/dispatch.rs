use {
    crate::{
        config::{Buyer, Config, SendPath},
        processor::{Launch, SnipeSignal},
        pump::{
            instructions::{self, CoinAccounts, CoinAccountsV2, StaticAccounts},
            quote::{self, CurveState},
        },
        sender::{fast::FastSenderPool, jito, rpc::RpcPool, tpu::TpuSender},
    },
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_hash::Hash,
    solana_instruction::Instruction,
    solana_message::{v0, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::sync::Arc,
    tokio::sync::{mpsc, RwLock},
};

pub struct BuyDispatcher {
    cfg: Arc<Config>,
    rpc: Arc<RpcPool>,
    fast: Arc<FastSenderPool>,
    tpu: Arc<TpuSender>,
    blockhash: Arc<RwLock<Hash>>,
    statics: Arc<StaticAccounts>,
    initial_curve: CurveState,
    /// v2 curves open against `Global.initial_virtual_quote_reserves` rather
    /// than the native-SOL reserve, so the two flows quote off different
    /// starting states.
    initial_curve_v2: CurveState,
}

impl BuyDispatcher {
    #[allow(clippy::too_many_arguments)] // wiring constructor, all of it needed
    pub fn new(
        cfg: Arc<Config>,
        rpc: Arc<RpcPool>,
        fast: Arc<FastSenderPool>,
        tpu: Arc<TpuSender>,
        blockhash: Arc<RwLock<Hash>>,
        statics: Arc<StaticAccounts>,
        initial_curve: CurveState,
        initial_curve_v2: CurveState,
    ) -> Self {
        Self {
            cfg,
            rpc,
            fast,
            tpu,
            blockhash,
            statics,
            initial_curve,
            initial_curve_v2,
        }
    }

    pub async fn run(self, mut signals: mpsc::Receiver<SnipeSignal>) {
        while let Some(signal) = signals.recv().await {
            self.dispatch(&signal).await;
        }
    }

    async fn dispatch(&self, signal: &SnipeSignal) {
        // v1 and v2 coins need different account sets and different buy
        // instructions; build whichever this launch calls for once, then share
        // it across every buyer.
        let coin = Arc::new(match signal.launch {
            Launch::V1 => Coin::V1(CoinAccounts::new(
                signal.mint,
                signal.bonding_curve,
                signal.associated_bonding_curve,
                &signal.creator,
            )),
            Launch::V2 { mayhem } => Coin::V2(CoinAccountsV2::new(
                signal.mint,
                signal.bonding_curve,
                &signal.creator,
                mayhem,
            )),
        });
        let blockhash = *self.blockhash.read().await;

        // Build and sign all buys in parallel — with 30 wallets, signing
        // serially would add avoidable milliseconds to the hot path.
        let jito_enabled = self.cfg.send_paths.contains(&SendPath::Jito);
        let buyer_count = self.cfg.buyers.len();
        let build_handles: Vec<_> = (0..buyer_count)
            .map(|i| {
                let cfg = Arc::clone(&self.cfg);
                let statics = Arc::clone(&self.statics);
                let coin = Arc::clone(&coin);
                let curve = if signal.launch.is_v2() {
                    self.initial_curve_v2
                } else {
                    self.initial_curve
                };
                let dev_buy = signal.dev_buy_lamports;
                // Jito bundles cap at 5 txs, so 30 buys become 6 bundles; the
                // tip rides on the last transaction of each bundle.
                let tip = (jito_enabled
                    && (i % jito::MAX_BUNDLE_SIZE == jito::MAX_BUNDLE_SIZE - 1
                        || i == buyer_count - 1))
                    .then_some(self.cfg.jito_tip_lamports);
                tokio::task::spawn_blocking(move || {
                    let buyer = &cfg.buyers[i];
                    build_buy_tx(
                        &cfg, &statics, &coin, buyer, &curve, dev_buy, blockhash, tip,
                    )
                })
            })
            .collect();

        let mut txs = Vec::with_capacity(build_handles.len());
        // Provider assignment must follow the surviving transactions, since a
        // failed build shifts every later index.
        let mut assignments = Vec::with_capacity(build_handles.len());
        for (i, handle) in build_handles.into_iter().enumerate() {
            match handle.await {
                Ok(Ok(tx)) => {
                    txs.push(tx);
                    assignments.push(self.cfg.buyers[i].provider);
                }
                Ok(Err(err)) => log::error!("[{}] buyer #{i} build failed: {err}", signal.mint),
                Err(err) => log::error!("[{}] buyer #{i} build panicked: {err}", signal.mint),
            }
        }
        if txs.is_empty() {
            log::error!("[{}] no transactions built, nothing to send", signal.mint);
            return;
        }

        log::info!(
            "[{}] {} ({}): dispatching {} buys via {:?} (dev_buy={} lamports)",
            signal.mint,
            signal.name,
            signal.symbol,
            txs.len(),
            self.cfg.send_paths,
            signal.dev_buy_lamports,
        );

        if self.cfg.dry_run {
            crate::sender::rpc::simulate_all(self.rpc.primary(), &txs).await;
            return;
        }

        // Every enabled path sends every transaction, concurrently. Duplicate
        // delivery is safe: a signature can land at most once on-chain, so the
        // fastest route wins and the others are no-ops.
        let mut paths: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
            Vec::new();
        if self.cfg.send_paths.contains(&SendPath::Rpc) {
            let rpc = Arc::clone(&self.rpc);
            let txs = txs.clone();
            paths.push(Box::pin(async move { rpc.spray(&txs).await }));
        }
        if self.cfg.send_paths.contains(&SendPath::Fast) {
            let fast = Arc::clone(&self.fast);
            let txs = txs.clone();
            let assignments = assignments.clone();
            paths.push(Box::pin(async move {
                fast.send_assigned(&txs, &assignments).await
            }));
        }
        if jito_enabled {
            let urls = self.cfg.jito_block_engine_urls.clone();
            let txs = txs.clone();
            paths.push(Box::pin(async move {
                jito::send_bundles(&urls, &txs).await;
            }));
        }
        // Direct QUIC to the next leaders' TPU. Targets and their connections
        // are kept resolved and warm by a background task, so this is a
        // single round trip and never awaits an RPC call.
        if self.cfg.send_paths.contains(&SendPath::Tpu) {
            let tpu = Arc::clone(&self.tpu);
            let txs = txs.clone();
            paths.push(Box::pin(async move { tpu.send(&txs).await }));
        }

        futures::future::join_all(paths).await;
    }
}

/// The per-coin accounts for whichever launch flow created this coin.
pub enum Coin {
    V1(CoinAccounts),
    V2(CoinAccountsV2),
}

#[allow(clippy::too_many_arguments)]
fn build_buy_tx(
    cfg: &Config,
    statics: &StaticAccounts,
    coin: &Coin,
    buyer: &Buyer,
    curve: &CurveState,
    dev_buy_lamports: u64,
    blockhash: Hash,
    jito_tip_lamports: Option<u64>,
) -> Result<VersionedTransaction, String> {
    let buyer_pk = buyer.keypair.pubkey();
    let min_tokens_out = quote::min_tokens_out(
        curve,
        buyer.buy_amount_lamports,
        dev_buy_lamports,
        cfg.slippage_bps,
    );

    let mut ixs: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(cfg.compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(buyer.priority_fee_micro_lamports),
    ];
    match coin {
        Coin::V1(coin) => ixs.extend([
            instructions::create_ata_idempotent(&buyer_pk, &buyer_pk, &coin.mint),
            instructions::buy_exact_sol_in(
                statics,
                coin,
                &buyer_pk,
                buyer.buy_amount_lamports,
                min_tokens_out,
                cfg.track_volume,
            ),
        ]),
        // A v2 buy spends *quote tokens*, so the lamports have to be wrapped
        // first: create the WSOL account, fund it, sync it so the program can
        // see the balance, then buy. The base ATA is Token-2022.
        Coin::V2(coin) => {
            let quote_ata = coin.quote_ata(&buyer_pk);
            ixs.extend([
                instructions::create_ata_idempotent_with_program(
                    &buyer_pk,
                    &buyer_pk,
                    &coin.quote_mint,
                    &coin.quote_token_program,
                ),
                solana_system_interface::instruction::transfer(
                    &buyer_pk,
                    &quote_ata,
                    buyer.buy_amount_lamports,
                ),
                instructions::sync_native(&quote_ata),
                instructions::create_ata_idempotent_with_program(
                    &buyer_pk,
                    &buyer_pk,
                    &coin.base_mint,
                    &coin.base_token_program,
                ),
                instructions::buy_exact_quote_in_v2(
                    statics,
                    coin,
                    &buyer_pk,
                    buyer.buy_amount_lamports,
                    min_tokens_out,
                ),
            ]);
            // Reclaim whatever the curve did not take, plus the account rent.
            if cfg.unwrap_after_buy {
                ixs.push(instructions::close_account(
                    &quote_ata, &buyer_pk, &buyer_pk,
                ));
            }
        }
    }
    // A fast-provider tip is part of the transaction, so it is baked in for the
    // provider this wallet was dealt. The same transaction still goes out over
    // plain RPC too — the tip only costs anything if this transaction is the
    // one that lands, and it can land at most once.
    if let Some(provider) = buyer.provider.and_then(|i| cfg.fast_providers.get(i)) {
        if let Some(tip_ix) = provider.tip_instruction(&buyer_pk) {
            ixs.push(tip_ix);
        }
    }
    if let Some(tip) = jito_tip_lamports {
        ixs.push(jito::tip_instruction(&buyer_pk, tip));
    }

    let message = v0::Message::try_compile(&buyer_pk, &ixs, &[], blockhash)
        .map_err(|e| format!("compile message: {e}"))?;
    VersionedTransaction::try_new(VersionedMessage::V0(message), &[&buyer.keypair])
        .map_err(|e| format!("sign tx: {e}"))
}
