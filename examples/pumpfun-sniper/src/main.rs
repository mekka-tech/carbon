mod config;
mod dispatch;
mod processor;
mod pump;
mod sender;

use {
    carbon_core::error::{CarbonResult, Error},
    carbon_log_metrics::LogMetrics,
    carbon_pumpfun_decoder::{accounts::global::Global, PumpfunDecoder},
    carbon_yellowstone_grpc_datasource::{
        YellowstoneGrpcClientConfig, YellowstoneGrpcGeyserClient,
    },
    config::Config,
    dispatch::BuyDispatcher,
    processor::SniperProcessor,
    pump::{instructions::StaticAccounts, pdas, quote::CurveState},
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_commitment_config::CommitmentConfig,
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
        "sniper starting: {} watched creator(s), {} buyer wallet(s), mode {:?}",
        cfg.watched_creators.len(),
        cfg.buyers.len(),
        cfg.send_mode
    );

    let rpc = Arc::new(RpcClient::new_with_commitment(
        cfg.rpc_url.clone(),
        CommitmentConfig::processed(),
    ));

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
    let statics = StaticAccounts::new(global.fee_recipient);

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
        Arc::clone(&rpc),
        Arc::clone(&blockhash),
        statics,
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
