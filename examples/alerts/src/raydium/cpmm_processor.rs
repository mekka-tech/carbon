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
  carbon_raydium_cpmm_decoder::{
      accounts::RaydiumCpmmAccount,
      instructions::{initialize::Initialize, RaydiumCpmmInstruction},
      RaydiumCpmmDecoder,
      PROGRAM_ID as RAYDIUM_CPMM_PROGRAM_ID,
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
