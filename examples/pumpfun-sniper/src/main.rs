mod config;
mod market;
mod tui_log;
mod console;
mod dashboard;
mod dispatch;
mod fills;
mod processor;
mod pump;
mod sell;
mod sender;
mod wallets;

use {
    carbon_core::error::{CarbonResult, Error},
    carbon_log_metrics::LogMetrics,
    carbon_jito_shredstream_grpc_datasource::JitoShredstreamGrpcClient,
    carbon_pumpfun_decoder::{accounts::global::Global, PumpfunDecoder},
    carbon_yellowstone_grpc_datasource::{
        YellowstoneGrpcClientConfig, YellowstoneGrpcGeyserClient,
    },
    config::{Config, DatasourceKind, MarketFeed, SendPath},
    dispatch::BuyDispatcher,
    processor::SniperProcessor,
    pump::{instructions::StaticAccounts, pdas, quote::CurveState},
    solana_pubkey::Pubkey,
    sender::{fast::FastSenderPool, rpc::RpcPool, tpu::TpuSender},
    std::{
        collections::{HashMap, HashSet},
        sync::Arc,
        time::Duration,
    },
    tokio::sync::{mpsc, RwLock},
    yellowstone_grpc_proto::geyser::{CommitmentLevel, SubscribeRequestFilterTransactions},
};

/// A Yellowstone subscription filtered server-side to the pump program at
/// `Processed`.
///
/// Shared by the primary feed on `DATASOURCE=yellowstone` and by the
/// market-only secondary feed alongside shredstream, because the two want the
/// same transactions: the market tracker needs precisely the pump transactions
/// detection needs, it just needs them carrying their execution metadata.
fn pumpfun_geyser_client(endpoint: String, x_token: Option<String>) -> YellowstoneGrpcGeyserClient {
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

    YellowstoneGrpcGeyserClient::new(
        endpoint,
        x_token,
        Some(CommitmentLevel::Processed),
        HashMap::default(),
        transaction_filters,
        Default::default(),
        Arc::new(RwLock::new(HashSet::new())),
        YellowstoneGrpcClientConfig::default(),
        None,
        None,
    )
}

#[tokio::main]
pub async fn main() -> CarbonResult<()> {
    dotenv::dotenv().ok();
    // In interactive mode stderr belongs to the TUI: env_logger and
    // carbon-log-metrics would otherwise interleave with the alternate screen
    // and shred the layout. Route records into a ring the panel renders, and
    // mirror everything to a file so nothing is lost.
    let interactive = std::env::args().any(|a| a == "--interactive" || a == "-i");
    let log_ring = if interactive {
        Some(tui_log::TuiLogger::install(
            Some("sniper.log"),
            std::env::var("RUST_LOG")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(log::Level::Info),
        ))
    } else {
        env_logger::init();
        None
    };

    // https://github.com/rustls/rustls/issues/1877
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls default provider");

    let cfg = Arc::new(Config::from_env().map_err(Error::Custom)?);
    log::info!(
        "sniper starting: datasource {:?}, {} watched creator(s), {} buyer wallet(s), {} rpc endpoint(s), paths {:?}{}",
        cfg.datasource,
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
    if global.create_v2_enabled && !cfg.snipe_v2 {
        log::warn!(
            "pump global has create_v2_enabled but SNIPE_V2 is off — create_v2 launches will be logged and skipped"
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
    // v2 coins price against the quote reserve. The curve arithmetic is the
    // same constant product; only the opening reserve differs.
    let initial_curve_v2 = CurveState {
        virtual_sol_reserves: global.initial_virtual_quote_reserves,
        ..initial_curve
    };
    if cfg.snipe_v2 && global.initial_virtual_quote_reserves == 0 {
        log::warn!(
            "SNIPE_V2 is on but Global.initial_virtual_quote_reserves is 0 — v2 quotes would be meaningless; v2 launches will be skipped"
        );
    }
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

    // Shared so the console can add/remove creators against the live pipeline.
    let watched: Arc<RwLock<HashSet<Pubkey>>> =
        Arc::new(RwLock::new(cfg.watched_creators.clone()));
    let market = Arc::new(RwLock::new(market::MarketTracker::default()));

    // Purchase tracking. The recorder is a plain channel sender, so the
    // dispatcher can never be delayed or failed by it; the log is read by the
    // console and the panel.
    let (fill_recorder, fill_log) = fills::start();

    let (signal_tx, signal_rx) = mpsc::channel(1_024);
    let dispatcher = BuyDispatcher::new(
        Arc::clone(&cfg),
        Arc::clone(&pool),
        Arc::clone(&fast),
        Arc::clone(&tpu),
        Arc::clone(&blockhash),
        Arc::clone(&statics),
        initial_curve,
        initial_curve_v2,
        fill_recorder,
    );
    tokio::spawn(Arc::new(dispatcher).run(signal_rx));

    // Both feeds produce Update::Transaction, so the rest of the pipeline is
    // identical; only the producer differs. Built separately because the two
    // datasource types are unrelated and the builder consumes `self`.
    let builder = carbon_core::pipeline::Pipeline::builder()
        .metrics(Arc::new(LogMetrics::new()))
        .instruction(
            PumpfunDecoder,
            SniperProcessor::new(
                Arc::clone(&cfg),
                Arc::clone(&watched),
                Arc::clone(&market),
                signal_tx,
            ),
        )
        .shutdown_strategy(carbon_core::pipeline::ShutdownStrategy::Immediate);

    let mut pipeline = match cfg.datasource {
        DatasourceKind::Yellowstone => {
            let geyser_url = cfg
                .geyser_url
                .clone()
                .ok_or_else(|| Error::Custom("GEYSER_URL missing".into()))?;
            log::info!(
                "market tracking is native on DATASOURCE=yellowstone: the feed carries real \
                 transaction metadata, so pump TradeEvent CPI events decode from it directly. No \
                 secondary feed is attached and MARKET_GEYSER_URL is not needed."
            );
            builder
                .datasource(pumpfun_geyser_client(geyser_url, cfg.x_token.clone()))
                .build()?
        }
        DatasourceKind::Shredstream => {
            let url = cfg
                .shredstream_url
                .clone()
                .ok_or_else(|| Error::Custom("SHREDSTREAM_URL missing".into()))?;
            log::info!(
                "shredstream: {} (x-token {})",
                url,
                if cfg.shredstream_x_token.is_some() {
                    "set"
                } else {
                    "ABSENT"
                }
            );
            // SubscribeEntriesRequest carries no filters, so this decodes every
            // transaction on the network rather than just the pump program's.
            //
            // ALT resolution is not optional here. Shreds carry no metadata, so
            // without it every v0 transaction using an address lookup table —
            // which is every pump.fun v2 launch — yields a truncated account
            // list, decodes to nothing, and reports success while doing it.
            // Restricted to pump transactions so we don't fetch lookup tables
            // for the whole network's traffic.
            let builder = builder.datasource(
                JitoShredstreamGrpcClient::new_with_x_token(url, cfg.shredstream_x_token.clone())
                    .with_alt_resolution(Arc::clone(&rpc))
                    .with_programs_of_interest(vec![pdas::PUMPFUN_PROGRAM_ID]),
            );

            // Optional second feed, for metadata only. Both datasources push
            // Update::Transaction into the SAME SniperProcessor, so a create
            // that appears on both is processed twice — harmless by
            // construction: `sniped_mints.contains(&mint)` is tested *before*
            // the insert, so whichever feed arrives first claims the mint and
            // the other returns without emitting a second snipe signal. Shreds
            // are pre-confirmation and the geyser is post-execution, so the
            // shred wins that race in practice and the buy keeps its timing.
            //
            // The guards do not drift between feeds either: `synthetic_meta` is
            // derived from the PRIMARY datasource, so the balance/freshness
            // guards stay skipped for updates from both, rather than being
            // enforced on geyser creates and not on shred ones.
            let builder = if let MarketFeed::Secondary(market_url) = &cfg.market_feed {
                log::info!(
                    "market feed: secondary yellowstone at {} (x-token {}), filtered to the pump \
                     program at Processed. It exists only to supply transaction metadata — \
                     shredstream stays the detection feed.",
                    market_url,
                    if cfg.market_x_token.is_some() {
                        "set"
                    } else {
                        "ABSENT"
                    }
                );
                builder.datasource(pumpfun_geyser_client(
                    market_url.clone(),
                    cfg.market_x_token.clone(),
                ))
            } else {
                // Unavailable — `Native` is unreachable here, it is what the
                // Yellowstone arm above resolves to.
                log::warn!(
                    "live price/volume will be UNAVAILABLE and the market panel will stay empty. \
                     Pump publishes every fill as a TradeEvent Anchor self-CPI, which is an INNER \
                     instruction materialised only when the transaction EXECUTES. Shreds carry \
                     the transaction as SUBMITTED, so the datasource leaves inner_instructions \
                     empty and no TradeEvent is ever decoded — this is a property of shreds, not \
                     a misconfiguration, and no amount of waiting will fill the panel. Detection \
                     is unaffected: `create` is a top-level instruction and is present in the \
                     submitted message. To get price/volume back, set MARKET_GEYSER_URL (plus \
                     MARKET_X_TOKEN, or X_TOKEN) and a second yellowstone stream will run \
                     alongside shredstream purely for metadata, leaving detection — and its \
                     pre-confirmation speed — on shredstream."
                );
                builder
            };

            builder.build()?
        }
    };

    // Interactive mode keeps the pipeline in the background so the operator can
    // inspect and exit a position in the same process that took it. Without it
    // the sniper buys and then has nothing to say.
    if interactive {
        tokio::spawn(async move {
            // `Ok` here is NOT success — `run()` returns Ok when its update
            // channel closes, i.e. when every datasource has stopped. Logging
            // only the Err case left the console rendering `*** LIVE ***` and
            // the watch list over a feed that had gone silent, which is the
            // worst possible failure: an armed-looking sniper that is deaf.
            match pipeline.run().await {
                Err(err) => log::error!("PIPELINE STOPPED: {err:?} — NOT SNIPING"),
                Ok(()) => log::error!(
                    "PIPELINE STOPPED: all datasources ended — NOT SNIPING. Restart required."
                ),
            }
        });
        console::run(
            Arc::clone(&cfg),
            Arc::clone(&rpc),
            Arc::clone(&watched),
            Arc::clone(&market),
            log_ring.unwrap_or_default(),
            Arc::clone(&blockhash),
            console::Buying {
                pool: Arc::clone(&pool),
                fast: Arc::clone(&fast),
                statics: Arc::clone(&statics),
                fills: fill_log,
                curve_v2: initial_curve_v2,
            },
        )
        .await;
        return Ok(());
    }

    pipeline.run().await?;

    Ok(())
}
