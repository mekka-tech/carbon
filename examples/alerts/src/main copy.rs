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
        instructions::{swap_base_in::SwapBaseIn, swap_base_out::SwapBaseOut, RaydiumAmmV4Instruction},
        RaydiumAmmV4Decoder,
        PROGRAM_ID as RAYDIUM_AMM_V4_PROGRAM_ID,
    },
    carbon_raydium_clmm_decoder::{
        accounts::RaydiumClmmAccount,
        instructions::{RaydiumClmmInstruction, swap_v2::SwapV2},
        RaydiumClmmDecoder,
        PROGRAM_ID as RAYDIUM_CLMM_PROGRAM_ID,
    },
    carbon_raydium_cpmm_decoder::{
        accounts::RaydiumCpmmAccount,
        instructions::RaydiumCpmmInstruction,
        RaydiumCpmmDecoder,
        PROGRAM_ID as RAYDIUM_CPMM_PROGRAM_ID,
    },
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
    env_logger::init();
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
        .account(RaydiumAmmV4Decoder, RaydiumAmmV4AccountProcessor)
        .account(RaydiumClmmDecoder, RaydiumClmmAccountProcessor)
        .account(RaydiumCpmmDecoder, RaydiumCpmmAccountProcessor)
        .shutdown_strategy(carbon_core::pipeline::ShutdownStrategy::Immediate)
        .build()?
        .run()
        .await?;

    Ok(())
}

pub struct RaydiumAmmV4InstructionProcessor;

#[async_trait]
impl Processor for RaydiumAmmV4InstructionProcessor {
    type InputType = (
        InstructionMetadata,
        DecodedInstruction<RaydiumAmmV4Instruction>,
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
            RaydiumAmmV4Instruction::Initialize2(init_pool) => {
                println!("AMM V4 Initialize2: signature: {signature}, init_pool: {init_pool:?}");
            }
            RaydiumAmmV4Instruction::Initialize(initialize) => {
                println!("AMM V4 Initialize: signature: {signature}, initialize: {initialize:?}");
            }
            RaydiumAmmV4Instruction::MonitorStep(monitor_step) => {
                println!("AMM V4 MonitorStep: signature: {signature}, monitor_step: {monitor_step:?}");
            }
            RaydiumAmmV4Instruction::Deposit(deposit) => {
                println!("AMM V4 Deposit: signature: {signature}, deposit: {deposit:?}");
            }
            RaydiumAmmV4Instruction::Withdraw(withdraw) => {
                println!("AMM V4 Withdraw: signature: {signature}, withdraw: {withdraw:?}");
            }
            RaydiumAmmV4Instruction::MigrateToOpenBook(migrate_to_open_book) => {
                println!("AMM V4 MigrateToOpenBook: signature: {signature}, migrate_to_open_book: {migrate_to_open_book:?}");
            }
            RaydiumAmmV4Instruction::SetParams(set_params) => {
                println!("AMM V4 SetParams: signature: {signature}, set_params: {set_params:?}");
            }
            RaydiumAmmV4Instruction::WithdrawPnl(withdraw_pnl) => {
                println!(
                    "AMM V4 WithdrawPnl: signature: {signature}, withdraw_pnl: {withdraw_pnl:?}"
                );
            }
            RaydiumAmmV4Instruction::WithdrawSrm(withdraw_srm) => {
                println!("AMM V4 WithdrawSrm: signature: {signature}, withdraw_srm: {withdraw_srm:?}");
            }
            RaydiumAmmV4Instruction::SwapBaseIn(swap_base_in) => {
                match SwapBaseIn::arrange_accounts(&accounts) {
                    Some(accounts) => {
                        println!(
                        "AMM V4 SwapBaseIn: signature: {signature}, swap_base_in: {swap_base_in:?}, accounts: {accounts:#?}",
                    );
                    }
                    None => log::error!(
                        "Failed to arrange accounts for SwapBaseIn {}",
                        accounts.len()
                    ),
                }
            }
            RaydiumAmmV4Instruction::PreInitialize(pre_initialize) => {
                println!(
                    "AMM V4 PreInitialize: signature: {signature}, pre_initialize: {pre_initialize:?}"
                );
            }
            RaydiumAmmV4Instruction::SwapBaseOut(swap_base_out) => {
                match SwapBaseOut::arrange_accounts(&accounts) {
                    Some(accounts) => {
                        println!(
                            "AMM V4 SwapBaseOut: signature: {signature}, swap_base_out: {swap_base_out:?}, accounts: {accounts:#?}",
                        );
                    }
                    None => log::error!(
                        "Failed to arrange accounts for AMM V4 SwapBaseOut {}",
                        accounts.len()
                    ),
                }
            }
            RaydiumAmmV4Instruction::SimulateInfo(simulate_info) => {
                println!(
                    "AMM V4 SimulateInfo: signature: {signature}, simulate_info: {simulate_info:?}"
                );
            }
            RaydiumAmmV4Instruction::AdminCancelOrders(admin_cancel_orders) => {
                println!(
                    "AMM V4 AdminCancelOrders: signature: {signature}, admin_cancel_orders: {admin_cancel_orders:?}"
                );
            }
            RaydiumAmmV4Instruction::CreateConfigAccount(create_config_account) => {
                println!(
                    "AMM V4 CreateConfigAccount: signature: {signature}, create_config_account: {create_config_account:?}"
                );
            }
            RaydiumAmmV4Instruction::UpdateConfigAccount(update_config_account) => {
                println!(
                    "AMM V4 UpdateConfigAccount: signature: {signature}, update_config_account: {update_config_account:?}"
                );
            }
        };

        Ok(())
    }
}

pub struct RaydiumAmmV4AccountProcessor;
#[async_trait]
impl Processor for RaydiumAmmV4AccountProcessor {
    type InputType = (AccountMetadata, DecodedAccount<RaydiumAmmV4Account>);

    async fn process(
        &mut self,
        data: Self::InputType,
        _metrics: Arc<MetricsCollection>,
    ) -> CarbonResult<()> {
        let account = data.1;

        match account.data {
            RaydiumAmmV4Account::AmmInfo(_pool) => {
                // println!("\nAccount: {:#?}\nPool: {:#?}", data.0.pubkey, pool);
            }
            _ => {
                // println!("\nUnnecessary Account: {:#?}", data.0.pubkey);
            }
        };

        Ok(())
    }
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
            RaydiumClmmInstruction::CreateAmmConfig(create_amm_cfg) => {
                println!(
                    "CLMM CreateAmmConfig: signature: {signature}, create_amm_cfg: {create_amm_cfg:?}"
                );
            }
            RaydiumClmmInstruction::UpdateAmmConfig(update_amm_cfg) => {
                println!(
                    "CLMM UpdateAmmConfig: signature: {signature}, update_amm_cfg: {update_amm_cfg:?}"
                );
            }
            RaydiumClmmInstruction::CreatePool(create_pool) => {
                println!("CLMM CreatePool: signature: {signature}, create_pool: {create_pool:?}");
            }
            RaydiumClmmInstruction::UpdatePoolStatus(update_pool_status) => {
                println!("CLMM UpdatePoolStatus: signature: {signature}, update_pool_status: {update_pool_status:?}");
            }
            RaydiumClmmInstruction::CreateOperationAccount(create_opperation_acc) => {
                println!("CLMM CreateOperationAccount: signature: {signature}, create_opperation_acc: {create_opperation_acc:?}");
            }
            RaydiumClmmInstruction::UpdateOperationAccount(update_opperation_acc) => {
                println!("CLMM UpdateOperationAccount: signature: {signature}, update_opperation_acc: {update_opperation_acc:?}");
            }
            RaydiumClmmInstruction::TransferRewardOwner(transfer_reward_owner) => {
                println!("CLMM TransferRewardOwner: signature: {signature}, transfer_reward_owner: {transfer_reward_owner:?}");
            }
            RaydiumClmmInstruction::InitializeReward(init_reward) => {
                println!(
                    "CLMM InitializeReward: signature: {signature}, init_reward: {init_reward:?}"
                );
            }
            RaydiumClmmInstruction::CollectRemainingRewards(collect_remaining_rewards) => {
                println!("CLMM CollectRemainingRewards: signature: {signature}, collect_remaining_rewards: {collect_remaining_rewards:?}");
            }
            RaydiumClmmInstruction::UpdateRewardInfos(update_reward_infos) => {
                println!("CLMM UpdateRewardInfos: signature: {signature}, update_reward_infos: {update_reward_infos:?}");
            }
            RaydiumClmmInstruction::SetRewardParams(set_reward_params) => {
                println!("CLMM SetRewardParams: signature: {signature}, set_reward_params: {set_reward_params:?}");
            }
            RaydiumClmmInstruction::CollectProtocolFee(collect_protocol_fee) => {
                println!("CLMM CollectProtocolFee: signature: {signature}, collect_protocol_fee: {collect_protocol_fee:?}");
            }
            RaydiumClmmInstruction::CollectFundFee(collect_fund_fee) => {
                println!("CLMM CollectFundFee: signature: {signature}, collect_fund_fee: {collect_fund_fee:?}");
            }
            RaydiumClmmInstruction::OpenPosition(open_position) => {
                println!(
                    "CLMM OpenPosition: signature: {signature}, open_position: {open_position:?}"
                );
            }
            RaydiumClmmInstruction::OpenPositionV2(open_position_v2) => {
                println!("CLMM OpenPositionV2: signature: {signature}, open_position_v2: {open_position_v2:?}");
            }
            RaydiumClmmInstruction::ClosePosition(close_position) => {
                println!(
                    "CLMM ClosePosition: signature: {signature}, close_position: {close_position:?}"
                );
            }
            RaydiumClmmInstruction::IncreaseLiquidity(increase_liq) => {
                println!(
                    "CLMM IncreaseLiquidity: signature: {signature}, increase_liq: {increase_liq:?}"
                );
            }
            RaydiumClmmInstruction::IncreaseLiquidityV2(increase_liq_v2) => {
                println!("CLMM IncreaseLiquidityV2: signature: {signature}, increase_liq_v2: {increase_liq_v2:?}");
            }
            RaydiumClmmInstruction::DecreaseLiquidity(decrease_liq) => {
                println!(
                    "CLMM DecreaseLiquidity: signature: {signature}, decrease_liq: {decrease_liq:?}"
                );
            }
            RaydiumClmmInstruction::DecreaseLiquidityV2(decrease_liq_v2) => {
                println!("CLMM DecreaseLiquidityV2: signature: {signature}, decrease_liq_v2: {decrease_liq_v2:?}");
            }
            RaydiumClmmInstruction::Swap(swap) => {
                println!("CLMM Swap: signature: {signature}, swap: {swap:?}");
            }
            RaydiumClmmInstruction::SwapV2(swap_v2) => match SwapV2::arrange_accounts(&accounts) {
                Some(accounts) => {
                    println!(
                            "CLMM SwapV2: signature: {signature}, swap_v2: {swap_v2:?}, accounts: {accounts:?}",
                        );
                }
                None => log::error!("Failed to arrange accounts for CLMM SwapV2 {}", accounts.len()),
            },
            RaydiumClmmInstruction::SwapRouterBaseIn(swap_base_in) => {
                println!(
                    "CLMM SwapRouterBaseIn: signature: {signature}, swap_base_in: {swap_base_in:?}"
                );
            }
            RaydiumClmmInstruction::ConfigChangeEvent(cfg_change_event) => {
                println!("CLMM ConfigChangeEvent: signature: {signature}, cfg_change_event: {cfg_change_event:?}");
            }
            RaydiumClmmInstruction::CreatePersonalPositionEvent(crete_personal_position) => {
                println!("CLMM CreatePersonalPositionEvent: signature: {signature}, crete_personal_position: {crete_personal_position:?}");
            }
            RaydiumClmmInstruction::IncreaseLiquidityEvent(increase_liq_event) => {
                println!("CLMM IncreaseLiquidityEvent: signature: {signature}, increase_liq_event: {increase_liq_event:?}");
            }
            RaydiumClmmInstruction::DecreaseLiquidityEvent(decrease_liq_event) => {
                println!("CLMM DecreaseLiquidityEvent: signature: {signature}, decrease_liq_event: {decrease_liq_event:?}");
            }
            RaydiumClmmInstruction::LiquidityCalculateEvent(liq_calc_event) => {
                println!("CLMM LiquidityCalculateEvent: signature: {signature}, liq_calc_event: {liq_calc_event:?}");
            }
            RaydiumClmmInstruction::CollectPersonalFeeEvent(collect_personal_fee_event) => {
                println!("CLMM CollectPersonalFeeEvent: signature: {signature}, collect_personal_fee_event: {collect_personal_fee_event:?}");
            }
            RaydiumClmmInstruction::UpdateRewardInfosEvent(update_reward_info_event) => {
                println!("CLMM UpdateRewardInfosEvent: signature: {signature}, update_reward_info_event: {update_reward_info_event:?}");
            }
            RaydiumClmmInstruction::PoolCreatedEvent(pool_create_event) => {
                println!("CLMM PoolCreatedEvent: signature: {signature}, pool_create_event: {pool_create_event:?}");
            }
            RaydiumClmmInstruction::CollectProtocolFeeEvent(collect_protocol_fee_event) => {
                println!("CLMM CollectProtocolFeeEvent: signature: {signature}, collect_protocol_fee_event: {collect_protocol_fee_event:?}");
            }
            RaydiumClmmInstruction::SwapEvent(swap_event) => {
                println!("CLMM SwapEvent: signature: {signature}, swap_event: {swap_event:?}");
            }
            RaydiumClmmInstruction::LiquidityChangeEvent(liq_change_event) => {
                println!("CLMM LiquidityChangeEvent: signature: {signature}, liq_change_event: {liq_change_event:?}");
            }
            RaydiumClmmInstruction::OpenPositionWithToken22Nft(open_position_with_token22_nft) => {
                println!("CLMM OpenPositionWithToken22Nft: signature: {signature}, open_position_with_token22_nft: {open_position_with_token22_nft:?}");
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
            RaydiumCpmmInstruction::CreateAmmConfig(create_amm_cfg) => {
                println!("CPMM CreateAmmConfig: signature: {signature}, create_amm_cfg: {create_amm_cfg:?}");
            }
            RaydiumCpmmInstruction::UpdateAmmConfig(update_amm_cfg) => {
                println!("CPMM UpdateAmmConfig: signature: {signature}, update_amm_cfg: {update_amm_cfg:?}");
            }
            RaydiumCpmmInstruction::UpdatePoolStatus(update_pool_status) => {
                println!("CPMM UpdatePoolStatus: signature: {signature}, update_pool_status: {update_pool_status:?}");
            }
            RaydiumCpmmInstruction::CollectProtocolFee(collect_protocol_fee) => {
                println!("CPMM CollectProtocolFee: signature: {signature}, collect_protocol_fee: {collect_protocol_fee:?}");
            }
            RaydiumCpmmInstruction::CollectFundFee(collect_fund_fee) => {
                println!("CPMM CollectFundFee: signature: {signature}, collect_fund_fee: {collect_fund_fee:?}");
            }
            RaydiumCpmmInstruction::Initialize(initialize) => {
                println!("CPMM Initialize: signature: {signature}, initialize: {initialize:?}");
            }
            RaydiumCpmmInstruction::Deposit(deposit) => {
                println!("CPMM Deposit: signature: {signature}, deposit: {deposit:?}");
            }
            RaydiumCpmmInstruction::Withdraw(withdraw) => {
                println!("CPMM Withdraw: signature: {signature}, withdraw: {withdraw:?}");
            }
            RaydiumCpmmInstruction::SwapBaseInput(swap_base_input) => {
                println!("CPMM SwapBaseInput: signature: {signature}, swap_base_input: {swap_base_input:?}");
            }
            RaydiumCpmmInstruction::SwapBaseOutput(swap_base_output) => {
                println!("CPMM SwapBaseOutput: signature: {signature}, swap_base_output: {swap_base_output:?}");
            }
            RaydiumCpmmInstruction::LpChangeEvent(lp_change_event) => {
                println!("CPMM LpChangeEvent: signature: {signature}, lp_change_event: {lp_change_event:?}");
            }
            RaydiumCpmmInstruction::SwapEvent(swap_event) => {
                println!("CPMM SwapEvent: signature: {signature}, swap_event: {swap_event:?}");
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
