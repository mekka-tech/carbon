use {
    // crate::events::{
    // events::{ProtocolType, SummarizedTokenBalance, SwapResult, SwapType},
    // rabbit::RabbitMQPublisher,
    // },
    async_trait::async_trait,
    carbon_core::{
        account::{AccountMetadata, DecodedAccount},
        deserialize::ArrangeAccounts,
        error::CarbonResult,
        instruction::{DecodedInstruction, InstructionMetadata, NestedInstruction},
        metrics::MetricsCollection,
        processor::Processor,
    },
    carbon_token_program_decoder::{
        accounts::TokenProgramAccount,
        instructions::{
            initialize_account::InitializeAccount, initialize_account2::InitializeAccount2,
            initialize_account3::InitializeAccount3, initialize_mint::InitializeMint,
            initialize_mint2::InitializeMint2, TokenProgramInstruction,
        },
    },
    serde_json::Result,
    std::sync::Arc,
};
pub struct TokenProcessor;

#[async_trait]
impl Processor for TokenProcessor {
    type InputType = (
        InstructionMetadata,
        DecodedInstruction<TokenProgramInstruction>,
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
            TokenProgramInstruction::InitializeAccount(initialize) => {
                match InitializeAccount::arrange_accounts(&accounts) {
                    Some(accounts) => {
                        println!("Token Initialize: signature: {signature}, initialize: {initialize:?}, accounts: {accounts:#?}",
                );
                    }
                    None => log::error!(
                        "Failed to arrange accounts for Token Initialize {}",
                        accounts.len()
                    ),
                }
            }
            TokenProgramInstruction::InitializeAccount2(initialize) => {
                match InitializeAccount2::arrange_accounts(&accounts) {
                    Some(accounts) => {
                        println!("Token Initialize2: signature: {signature}, initialize: {initialize:?}, accounts: {accounts:#?}",
                );
                    }
                    None => log::error!(
                        "Failed to arrange accounts for Token Initialize2 {}",
                        accounts.len()
                    ),
                }
            }
            TokenProgramInstruction::InitializeAccount3(initialize) => {
                match InitializeAccount3::arrange_accounts(&accounts) {
                    Some(accounts) => {
                        println!("Token Initialize3: signature: {signature}, initialize: {initialize:?}, accounts: {accounts:#?}",
                );
                    }
                    None => log::error!(
                        "Failed to arrange accounts for Token Initialize3 {}",
                        accounts.len()
                    ),
                }
            }
            TokenProgramInstruction::InitializeMint(initialize) => {
                match InitializeMint::arrange_accounts(&accounts) {
                    Some(accounts) => {
                        println!("Token InitializeMint: signature: {signature}, initialize: {initialize:?}, accounts: {accounts:#?}",
                );
                    }
                    None => log::error!(
                        "Failed to arrange accounts for Token InitializeMint {}",
                        accounts.len()
                    ),
                }
            }
            TokenProgramInstruction::InitializeMint2(initialize) => {
                match InitializeMint2::arrange_accounts(&accounts) {
                    Some(accounts) => {
                        println!("Token InitializeMint2: signature: {signature}, initialize: {initialize:?}, accounts: {accounts:#?}",
                );
                    }
                    None => log::error!(
                        "Failed to arrange accounts for Token InitializeMint2 {}",
                        accounts.len()
                    ),
                }
            }
            _ => {
                // Ignored
            }
        };

        Ok(())
    }
}

pub struct TokenAccountProcessor;
#[async_trait]
impl Processor for TokenAccountProcessor {
    type InputType = (AccountMetadata, DecodedAccount<TokenProgramAccount>);

    async fn process(
        &mut self,
        data: Self::InputType,
        _metrics: Arc<MetricsCollection>,
    ) -> CarbonResult<()> {
        let account = data.1;

        match account.data {
            TokenProgramAccount::Mint(mint) => {
                println!("Mint: {:#?}", mint);
            }
            _ => {
                // println!("\nUnnecessary Account: {:#?}", data.0.pubkey);
            }
        };

        Ok(())
    }
}
