mod raydium {
    pub mod raydium_amm_v4_processors;
}
mod events {
    pub mod events;
}

use events::events::{SummarizedTokenBalance, NewPoolEventPayload};
use raydium::raydium_amm_v4_processors::{RaydiumAmmV4InstructionProcessor, RaydiumAmmV4AccountProcessor};

use {
    async_trait::async_trait,
    carbon_core::{
        account::{AccountMetadata, DecodedAccount},
        deserialize::ArrangeAccounts,
        error::CarbonResult,
        instruction::{DecodedInstruction, InstructionMetadata, NestedInstruction},
        metrics::MetricsCollection,
        processor::Processor,
    },
    carbon_raydium_amm_v4_decoder::{
        accounts::RaydiumAmmV4Account,
        instructions::{initialize2::Initialize2, swap_base_in::SwapBaseIn, swap_base_out::SwapBaseOut, RaydiumAmmV4Instruction},
        RaydiumAmmV4Decoder,
        PROGRAM_ID as RAYDIUM_AMM_V4_PROGRAM_ID,
    },
    carbon_raydium_clmm_decoder::{
        accounts::RaydiumClmmAccount,
        instructions::{create_pool::CreatePool, RaydiumClmmInstruction},
        RaydiumClmmDecoder,
        PROGRAM_ID as RAYDIUM_CLMM_PROGRAM_ID,
    },
    carbon_raydium_cpmm_decoder::{
        accounts::RaydiumCpmmAccount,
        instructions::{initialize::Initialize, RaydiumCpmmInstruction},
        RaydiumCpmmDecoder,
        PROGRAM_ID as RAYDIUM_CPMM_PROGRAM_ID,
    },
    carbon_pumpfun_decoder::{
        instructions::PumpfunInstruction,
        PumpfunDecoder,
        PROGRAM_ID as PUMPFUN_PROGRAM_ID,
    },
    carbon_yellowstone_grpc_datasource::YellowstoneGrpcGeyserClient,
    std::{
        collections::{HashMap, HashSet},
        env,
        sync::Arc,
    },
    solana_sdk::{ native_token::LAMPORTS_PER_SOL },
    solana_transaction_status::TransactionTokenBalance,
    tokio::sync::RwLock,
    yellowstone_grpc_proto::geyser::{
        CommitmentLevel, SubscribeRequestFilterAccounts, SubscribeRequestFilterTransactions,
    },
    bullmq_rust::queue_service::QueueService,
    bullmq_rust::job_model::JobData,
    bullmq_rust::config_service::ConfigService,
    chrono::Utc,
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
    account_filters.insert(
        "raydium_amm_v4_account_filter".to_string(),
        SubscribeRequestFilterAccounts {
            account: vec![],
            owner: vec![RAYDIUM_AMM_V4_PROGRAM_ID.to_string().clone()],
            filters: vec![],
            nonempty_txn_signature: None,
        },
    );
    account_filters.insert(
        "raydium_clmm_account_filter".to_string(),
        SubscribeRequestFilterAccounts {
            account: vec![],
            owner: vec![RAYDIUM_CLMM_PROGRAM_ID.to_string().clone()],
            filters: vec![],
            nonempty_txn_signature: None,
        },
    );
    account_filters.insert(
        "raydium_cpmm_account_filter".to_string(),
        SubscribeRequestFilterAccounts {
            account: vec![],
            owner: vec![RAYDIUM_CPMM_PROGRAM_ID.to_string().clone()],
            filters: vec![],
            nonempty_txn_signature: None,
        },
    );
    account_filters.insert(
        "raydium_cpmm_account_filter".to_string(),
        SubscribeRequestFilterAccounts {
            account: vec![],
            owner: vec![RAYDIUM_CPMM_PROGRAM_ID.to_string().clone()],
            filters: vec![],
            nonempty_txn_signature: None,
        },
    );

    let mut transaction_filters: HashMap<String, SubscribeRequestFilterTransactions> =
        HashMap::new();

    transaction_filters.insert("raydium_amm_v4_transaction_filter".to_string(), SubscribeRequestFilterTransactions {
        vote: Some(false),
        failed: Some(false),
        account_include: vec![],
        account_exclude: vec![],
        account_required: vec![RAYDIUM_AMM_V4_PROGRAM_ID.to_string().clone()],
        signature: None,
    });

    transaction_filters.insert("raydium_clmm_transaction_filter".to_string(), SubscribeRequestFilterTransactions {
        vote: Some(false),
        failed: Some(false),
        account_include: vec![],
        account_exclude: vec![],
        account_required: vec![RAYDIUM_CLMM_PROGRAM_ID.to_string().clone()],
        signature: None,
    });

    transaction_filters.insert(
        "raydium_cpmm_transaction_filter".to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: Some(false),
            account_include: vec![],
            account_exclude: vec![],
            account_required: vec![RAYDIUM_CPMM_PROGRAM_ID.to_string().clone()],
            signature: None,
        },
    );

    transaction_filters.insert("pumpfun_transaction_filter".to_string(), SubscribeRequestFilterTransactions {
        vote: Some(false),
        failed: Some(false),
        account_include: vec![],
        account_exclude: vec![],
        account_required: vec![PUMPFUN_PROGRAM_ID.to_string().clone()],
        signature: None,
    });

    let yellowstone_grpc = YellowstoneGrpcGeyserClient::new(
        env::var("GEYSER_URL").unwrap_or_default(),
        env::var("X_TOKEN").ok(),
        Some(CommitmentLevel::Confirmed),
        account_filters,
        transaction_filters,
        Arc::new(RwLock::new(HashSet::new())),
    );

    carbon_core::pipeline::Pipeline::builder()
        .datasource(yellowstone_grpc)
        .instruction(RaydiumAmmV4Decoder, RaydiumAmmV4InstructionProcessor)
        .instruction(RaydiumClmmDecoder, RaydiumClmmInstructionProcessor)
        .instruction(RaydiumCpmmDecoder, RaydiumCpmmInstructionProcessor)
        .instruction(PumpfunDecoder, PumpfunInstructionProcessor)
        .account(RaydiumAmmV4Decoder, RaydiumAmmV4AccountProcessor)
        .account(RaydiumClmmDecoder, RaydiumClmmAccountProcessor)
        .account(RaydiumCpmmDecoder, RaydiumCpmmAccountProcessor)
        .shutdown_strategy(carbon_core::pipeline::ShutdownStrategy::Immediate)
        .build()?
        .run()
        .await?;

    Ok(())
}

pub struct RaydiumClmmAccountProcessor;
#[async_trait]
impl Processor for RaydiumClmmAccountProcessor {
    type InputType = (AccountMetadata, DecodedAccount<RaydiumClmmAccount>);

    async fn process(
        &mut self,
        data: Self::InputType,
        _metrics: Arc<MetricsCollection>,
    ) -> CarbonResult<()> {
        let account = data.1;

        match account.data {
            RaydiumClmmAccount::AmmConfig(_amm_cfg) => {
                // println!("\nAccount: {:#?}\nPool: {:#?}", data.0.pubkey, pool);
            }
            _ => {
                // println!("\nUnnecessary Account: {:#?}", data.0.pubkey);
            }
        };

        Ok(())
    }
}

pub struct RaydiumClmmInstructionProcessor;

#[async_trait]
impl Processor for RaydiumClmmInstructionProcessor {
    type InputType = (
        InstructionMetadata,
        DecodedInstruction<RaydiumClmmInstruction>,
        Vec<NestedInstruction>,
    );

    async fn process(
        &mut self,
        (metadata, instruction, _nested_instructions): Self::InputType,
        _metrics: Arc<MetricsCollection>,
    ) -> CarbonResult<()> {
        let signature = metadata.transaction_metadata.signature;
        let accounts = instruction.accounts;

        match instruction.data {
            RaydiumClmmInstruction::CreatePool(create_pool) => match CreatePool::arrange_accounts(&accounts) {
                Some(accounts) => {
                    println!("CLMM CreatePool: signature: {signature}, create_pool: {create_pool:?}, accounts: {accounts:#?}",
                    );
                }
                None => println!("Failed to arrange accounts for CLMM CreatePool {}", accounts.len()),
            },
            _ => {

            }
        };

        Ok(())
    }
}

pub struct RaydiumCpmmInstructionProcessor;

#[async_trait]
impl Processor for RaydiumCpmmInstructionProcessor {
    type InputType = (InstructionMetadata, DecodedInstruction<RaydiumCpmmInstruction>, Vec<NestedInstruction>);

    async fn process(
        &mut self,
        (metadata, instruction, _nested_instructions): Self::InputType,
        _metrics: Arc<MetricsCollection>,
    ) -> CarbonResult<()> {
        let signature = metadata.transaction_metadata.signature;
        let accounts = instruction.accounts;

        match instruction.data {
            RaydiumCpmmInstruction::Initialize(initialize) => match Initialize::arrange_accounts(&accounts) {
                Some(accounts) => {
                    println!("CPMM Initialize: signature: {signature}, initialize: {initialize:?}, accounts: {accounts:#?}",
                    );
                }
                None => log::error!("Failed to arrange accounts for CPMM Initialize {}", accounts.len()),
            },
            _ => {
                // Ignored
            }
        };

        Ok(())
    }
}

pub struct RaydiumCpmmAccountProcessor;
#[async_trait]
impl Processor for RaydiumCpmmAccountProcessor {
    type InputType = (AccountMetadata, DecodedAccount<RaydiumCpmmAccount>);

    async fn process(
        &mut self,
        data: Self::InputType,
        _metrics: Arc<MetricsCollection>,
    ) -> CarbonResult<()> {
        let account = data.1;

        match account.data {
            RaydiumCpmmAccount::AmmConfig(_amm_cfg) => {
                // println!("\nAccount: {:#?}\nPool: {:#?}", data.0.pubkey, pool);
            }
            _ => {
                // println!("\nUnnecessary Account: {:#?}", data.0.pubkey);
            }
        };

        Ok(())
    }
}

pub struct PumpfunInstructionProcessor;

#[async_trait]
impl Processor for PumpfunInstructionProcessor {
    type InputType = (InstructionMetadata, DecodedInstruction<PumpfunInstruction>, Vec<NestedInstruction>);

    async fn process(
        &mut self,
        (metadata, instruction, _nested_instructions): Self::InputType,
        _metrics: Arc<MetricsCollection>,
    ) -> CarbonResult<()> {
        let signature = metadata.transaction_metadata.signature;
        let accounts = instruction.accounts;

        match instruction.data {
            PumpfunInstruction::CreateEvent(create_event) => {
                println!("\nNew token created: {:#?}", create_event);
            }
            PumpfunInstruction::TradeEvent(trade_event) => {
                if trade_event.sol_amount > 10 * LAMPORTS_PER_SOL {
                    println!("\nBig trade occured: {:#?}", trade_event);
                }
            }
            PumpfunInstruction::CompleteEvent(complete_event) => {
                println!("\nBonded: {:#?}", complete_event);
            }
            _ => {
                // Ignored
            }
        };

        Ok(())
    }
} 