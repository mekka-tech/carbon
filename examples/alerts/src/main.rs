mod raydium {
    pub mod amm_v4_processor;
    pub mod clmm_processor;
    pub mod cpmm_processor;
}
mod pumpfun {
    pub mod pumpfun_processor;
    pub mod order_book;
    pub mod swap;
}
mod token {
    pub mod token_processor;
}
mod events {
    pub mod events;
    pub mod rabbit;
}

use token::token_processor::{TokenProcessor, TokenAccountProcessor};
use pumpfun::pumpfun_processor::PumpfunInstructionProcessor;
use pumpfun::swap::SwapPublisher;
use raydium::amm_v4_processor::{RaydiumAmmV4AccountProcessor, RaydiumAmmV4InstructionProcessor};
use raydium::clmm_processor::{RaydiumClmmAccountProcessor, RaydiumClmmInstructionProcessor};
use raydium::cpmm_processor::{RaydiumCpmmAccountProcessor, RaydiumCpmmInstructionProcessor};
use tungstenite::{connect, Message};
use {
    crate::events::rabbit::RabbitMQPublisher,
    carbon_core::error::CarbonResult,
    carbon_pumpfun_decoder::{PumpfunDecoder, PROGRAM_ID as PUMPFUN_PROGRAM_ID},
    carbon_raydium_amm_v4_decoder::{RaydiumAmmV4Decoder, PROGRAM_ID as RAYDIUM_AMM_V4_PROGRAM_ID},
    carbon_raydium_clmm_decoder::{RaydiumClmmDecoder, PROGRAM_ID as RAYDIUM_CLMM_PROGRAM_ID},
    carbon_raydium_cpmm_decoder::{RaydiumCpmmDecoder, PROGRAM_ID as RAYDIUM_CPMM_PROGRAM_ID},
    carbon_token_program_decoder::{TokenProgramDecoder, PROGRAM_ID as TOKEN_PROGRAM_ID},
    carbon_yellowstone_grpc_datasource::YellowstoneGrpcGeyserClient,
    std::{
        collections::{HashMap, HashSet},
        env,
        sync::Arc,
    },
    tokio::sync::RwLock,
    yellowstone_grpc_proto::geyser::{
        CommitmentLevel, SubscribeRequestFilterAccounts, SubscribeRequestFilterTransactions,
    },
};

#[tokio::main]
pub async fn main() -> CarbonResult<()> {
    env_logger::Builder::from_default_env()
        .format(|buf, record| {
            use std::io::Write;
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            writeln!(
                buf,
                "[{}] {} - {}: {}",
                timestamp,
                record.level(),
                record.target(),
                record.args()
            )
        })
        .init();
    dotenv::dotenv().ok();

    let mut account_filters: HashMap<String, SubscribeRequestFilterAccounts> = HashMap::new();
    // account_filters.insert(
    //     "raydium_amm_v4_account_filter".to_string(),
    //     SubscribeRequestFilterAccounts {
    //         account: vec![],
    //         owner: vec![RAYDIUM_AMM_V4_PROGRAM_ID.to_string().clone()],
    //         filters: vec![],
    //         nonempty_txn_signature: None,
    //     },
    // );
    // account_filters.insert(
    //     "raydium_clmm_account_filter".to_string(),
    //     SubscribeRequestFilterAccounts {
    //         account: vec![],
    //         owner: vec![RAYDIUM_CLMM_PROGRAM_ID.to_string().clone()],
    //         filters: vec![],
    //         nonempty_txn_signature: None,
    //     },
    // );
    // account_filters.insert(
    //     "raydium_cpmm_account_filter".to_string(),
    //     SubscribeRequestFilterAccounts {
    //         account: vec![],
    //         owner: vec![RAYDIUM_CPMM_PROGRAM_ID.to_string().clone()],
    //         filters: vec![],
    //         nonempty_txn_signature: None,
    //     },
    // );
    // account_filters.insert(
    //     "raydium_cpmm_account_filter".to_string(),
    //     SubscribeRequestFilterAccounts {
    //         account: vec![],
    //         owner: vec![RAYDIUM_CPMM_PROGRAM_ID.to_string().clone()],
    //         filters: vec![],
    //         nonempty_txn_signature: None,
    //     },
    // );
    // account_filters.insert(
    //     "raydium_cpmm_account_filter".to_string(),
    //     SubscribeRequestFilterAccounts {
    //         account: vec![],
    //         owner: vec![TOKEN_PROGRAM_ID.to_string().clone()],
    //         filters: vec![],
    //         nonempty_txn_signature: None,
    //     },
    // );

    let mut transaction_filters: HashMap<String, SubscribeRequestFilterTransactions> =
        HashMap::new();

    // transaction_filters.insert(
    //     "raydium_amm_v4_transaction_filter".to_string(),
    //     SubscribeRequestFilterTransactions {
    //         vote: Some(false),
    //         failed: Some(false),
    //         account_include: vec![],
    //         account_exclude: vec![],
    //         account_required: vec![RAYDIUM_AMM_V4_PROGRAM_ID.to_string().clone()],
    //         signature: None,
    //     },
    // );

    // transaction_filters.insert(
    //     "raydium_clmm_transaction_filter".to_string(),
    //     SubscribeRequestFilterTransactions {
    //         vote: Some(false),
    //         failed: Some(false),
    //         account_include: vec![],
    //         account_exclude: vec![],
    //         account_required: vec![RAYDIUM_CLMM_PROGRAM_ID.to_string().clone()],
    //         signature: None,
    //     },
    // );

    // transaction_filters.insert(
    //     "raydium_cpmm_transaction_filter".to_string(),
    //     SubscribeRequestFilterTransactions {
    //         vote: Some(false),
    //         failed: Some(false),
    //         account_include: vec![],
    //         account_exclude: vec![],
    //         account_required: vec![RAYDIUM_CPMM_PROGRAM_ID.to_string().clone()],
    //         signature: None,
    //     },
    // );

    transaction_filters.insert(
        "pumpfun_transaction_filter".to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: Some(false),
            account_include: vec![PUMPFUN_PROGRAM_ID.to_string().clone()],
            account_exclude: vec![],
            account_required: vec![],
            signature: None,
        },
    );

    // transaction_filters.insert(
    //     "token_transaction_filter".to_string(),
    //     SubscribeRequestFilterTransactions {
    //         vote: Some(false),
    //         failed: Some(false),
    //         account_include: vec![],
    //         account_exclude: vec![],
    //         account_required: vec![TOKEN_PROGRAM_ID.to_string().clone()],
    //         signature: None,
    //     },
    // );

    

    let yellowstone_grpc = YellowstoneGrpcGeyserClient::new(
        env::var("GEYSER_URL").unwrap_or_default(),
        env::var("X_TOKEN").ok(),
        Some(CommitmentLevel::Processed),
        account_filters,
        transaction_filters,
        Arc::new(RwLock::new(HashSet::new())),
    );

    // RabbitMQPublisher::init(
    //     env::var("RABBITMQ_HOST").unwrap_or_default(),
    //     env::var("RABBITMQ_PORT")
    //         .unwrap_or_default()
    //         .parse::<u16>()
    //         .unwrap_or(5672),
    //     env::var("RABBITMQ_USER").unwrap_or_default(),
    //     env::var("RABBITMQ_PASSWORD").unwrap_or_default(),
    //     env::var("RABBITMQ_VHOST").unwrap_or_default(),
    // )
    // .await?;

    carbon_core::pipeline::Pipeline::builder()
        .datasource(yellowstone_grpc)
        // .instruction(RaydiumAmmV4Decoder, RaydiumAmmV4InstructionProcessor)
        // .instruction(RaydiumClmmDecoder, RaydiumClmmInstructionProcessor)
        // .instruction(RaydiumCpmmDecoder, RaydiumCpmmInstructionProcessor)
        .instruction(PumpfunDecoder, PumpfunInstructionProcessor)
        // .instruction(TokenProgramDecoder, TokenProcessor)
        // .account(RaydiumAmmV4Decoder, RaydiumAmmV4AccountProcessor)
        // .account(RaydiumClmmDecoder, RaydiumClmmAccountProcessor)
        // .account(RaydiumCpmmDecoder, RaydiumCpmmAccountProcessor)
        // .account(TokenProgramDecoder, TokenAccountProcessor)
        .shutdown_strategy(carbon_core::pipeline::ShutdownStrategy::Immediate)
        .build()?
        .run()
        .await?;

    Ok(())
}
