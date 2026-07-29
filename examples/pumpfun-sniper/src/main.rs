mod config;
mod dispatch;
mod processor;
mod pump;
mod sender;
mod wallets;

use {
    carbon_core::error::{CarbonResult, Error},
    carbon_log_metrics::LogMetrics,
    carbon_pumpfun_decoder::{accounts::global::Global, PumpfunDecoder},
    carbon_yellowstone_grpc_datasource::{
        YellowstoneGrpcClientConfig, YellowstoneGrpcGeyserClient,
    },
    config::{Config, SendPath},
    dispatch::BuyDispatcher,
    processor::SniperProcessor,
    pump::{instructions::StaticAccounts, pdas, quote::CurveState},
    sender::{fast::FastSenderPool, rpc::RpcPool, tpu::TpuSender},
    std::{
        collections::{HashMap, HashSet},
        sync::Arc,
        time::Duration,
    },
    tokio::sync::{mpsc, RwLock},
    yellowstone_grpc_proto::geyser::{CommitmentLevel, SubscribeRequestFilterTransactions},
};

#[tokio::main]
pub async fn main() -> CarbonResult<()> {
    dotenv::dotenv().ok();
    env_logger::init();

    // https://github.com/rustls/rustls/issues/1877
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls default provider");

    let cfg = Arc::new(Config::from_env().map_err(Error::Custom)?);
    log::info!(
        "sniper starting: {} watched creator(s), {} buyer wallet(s), {} rpc endpoint(s), paths {:?}{}",
        cfg.watched_creators.len(),
        cfg.buyers.len(),
        cfg.rpc_urls.len(),
        cfg.send_paths,
        if cfg.dry_run { " (DRY RUN)" } else { "" }
    );

    let pool = Arc::new(RpcPool::new(&cfg.rpc_urls));
    let rpc = Arc::clone(pool.primary());
    let fast = Arc::new(FastSenderPool::new(cfg.fast_providers.clone()));
    if !fast.is_empty() {
        log::info!("{} fast provider(s) configured", fast.len());
        fast.warm().await;
    }

    // Direct-TPU path. Constructed unconditionally so the dispatcher always has
    // one, but it only does work — and only costs an RPC call every few hundred
    // ms — when `SEND_PATHS` enables it.
    let tpu = Arc::new(TpuSender::new(Arc::clone(&rpc), cfg.tpu_leaders_ahead));
    if cfg.send_paths.contains(&SendPath::Tpu) {
        if tpu.is_enabled() {
            // Resolve and pre-warm once before the pipeline starts, so the
            // first snipe of the run isn't the one paying for a QUIC handshake.
            tpu.refresh().await;
            log::info!(
                "tpu direct path enabled: {} upcoming leader(s), {} connection(s) warm",
                cfg.tpu_leaders_ahead,
                tpu.targets().len()
            );
            tpu.spawn_refresher();
        } else {
            log::warn!("SEND_PATHS includes 'tpu' but TPU_LEADERS_AHEAD=0, path disabled");
        }
    }

    // Fetch the pump Global account once: fee bps + initial virtual reserves
    // feed the quote; fee_recipient feeds the buy instruction.
    let global_account = rpc
        .get_account(&pdas::global())
        .await
        .map_err(|e| Error::Custom(format!("failed to fetch pump global account: {e}")))?;
    let global = Global::decode(&global_account.data)
        .ok_or_else(|| Error::Custom("failed to decode pump global account".into()))?;
    if global.create_v2_enabled {
        log::warn!(
            "pump global has create_v2_enabled — coins launched via create_v2 will be logged and skipped (v1 buys only)"
        );
    }

    // Fail fast if our fee_config PDA derivation doesn't match an on-chain
    // account — every buy would fail otherwise.
    rpc.get_account(&pdas::fee_config()).await.map_err(|e| {
        Error::Custom(format!(
            "fee_config PDA {} not found on-chain ({e}) — derivation needs updating",
            pdas::fee_config()
        ))
    })?;

    let initial_curve = CurveState {
        virtual_sol_reserves: global.initial_virtual_sol_reserves,
        virtual_token_reserves: global.initial_virtual_token_reserves,
        protocol_fee_bps: global.fee_basis_points,
        creator_fee_bps: global.creator_fee_basis_points,
    };
    // Take the buyback fee recipients from the live Global account rather than
    // the compiled-in snapshot: `update_buyback_config` can rotate them, and a
    // stale list fails every buy with BuybackFeeRecipientNotAuthorized (6057).
    let statics = Arc::new(StaticAccounts::from_global(&global));

    // Every wallet must be able to cover its buy plus fees and rent — catch
    // that now, not mid-launch.
    let underfunded = wallets::preflight_balances(&cfg, &rpc)
        .await
        .map_err(Error::Custom)?;
    if !underfunded.is_empty() && !cfg.dry_run {
        return Err(Error::Custom(format!(
            "{} of {} buyer wallets are underfunded — fund them or remove them before going live",
            underfunded.len(),
            cfg.buyers.len()
        )));
    }

    // Warm blockhash so the dispatch hot path never awaits an RPC round trip.
    let initial_blockhash = rpc
        .get_latest_blockhash()
        .await
        .map_err(|e| Error::Custom(format!("failed to fetch initial blockhash: {e}")))?;
    let blockhash = Arc::new(RwLock::new(initial_blockhash));
    {
        let blockhash = Arc::clone(&blockhash);
        let rpc = Arc::clone(&rpc);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(400)).await;
                match rpc.get_latest_blockhash().await {
                    Ok(hash) => *blockhash.write().await = hash,
                    Err(err) => log::warn!("blockhash refresh failed: {err}"),
                }
            }
        });
    }

    let (signal_tx, signal_rx) = mpsc::channel(1_024);
    let dispatcher = BuyDispatcher::new(
        Arc::clone(&cfg),
        Arc::clone(&pool),
        Arc::clone(&fast),
        Arc::clone(&tpu),
        Arc::clone(&blockhash),
        Arc::clone(&statics),
        initial_curve,
    );
    tokio::spawn(dispatcher.run(signal_rx));

    let mut transaction_filters = HashMap::new();
    transaction_filters.insert(
        "pumpfun".to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: Some(false),
            account_include: vec![pdas::PUMPFUN_PROGRAM_ID.to_string()],
            account_exclude: vec![],
            account_required: vec![],
            signature: None,
        },
    );

    let datasource = YellowstoneGrpcGeyserClient::new(
        cfg.geyser_url.clone(),
        cfg.x_token.clone(),
        Some(CommitmentLevel::Processed),
        HashMap::default(),
        transaction_filters,
        Default::default(),
        Arc::new(RwLock::new(HashSet::new())),
        YellowstoneGrpcClientConfig::default(),
        None,
        None,
    );

    carbon_core::pipeline::Pipeline::builder()
        .datasource(datasource)
        .metrics(Arc::new(LogMetrics::new()))
        .instruction(
            PumpfunDecoder,
            SniperProcessor::new(Arc::clone(&cfg), signal_tx),
        )
        .shutdown_strategy(carbon_core::pipeline::ShutdownStrategy::Immediate)
        .build()?
        .run()
        .await?;

    Ok(())
}
