use {
    crate::events::{
        events::{ProtocolType, SummarizedTokenBalance, SwapResult, SwapType},
        rabbit::RabbitMQPublisher,
    },
    async_trait::async_trait,
    carbon_core::{
        account::{AccountMetadata, DecodedAccount},
        deserialize::ArrangeAccounts,
        error::CarbonResult,
        instruction::{DecodedInstruction, InstructionMetadata, NestedInstruction},
        metrics::MetricsCollection,
        processor::Processor,
    },
    carbon_raydium_clmm_decoder::{
        accounts::RaydiumClmmAccount,
        instructions::{create_pool::CreatePool, RaydiumClmmInstruction},
        RaydiumClmmDecoder, PROGRAM_ID as RAYDIUM_CLMM_PROGRAM_ID,
    },
    chrono::Utc,
    serde_json::Result,
    solana_sdk::instruction::AccountMeta,
    solana_transaction_status::{TransactionStatusMeta, TransactionTokenBalance},
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
            RaydiumClmmInstruction::CreatePool(create_pool) => {
                match CreatePool::arrange_accounts(&accounts) {
                    Some(accounts) => {
                        println!("CLMM CreatePool: signature: {signature}, create_pool: {create_pool:?}, accounts: {accounts:#?}",
                    );
                    }
                    None => println!(
                        "Failed to arrange accounts for CLMM CreatePool {}",
                        accounts.len()
                    ),
                }
            }
            _ => {}
        };

        Ok(())
    }
}
