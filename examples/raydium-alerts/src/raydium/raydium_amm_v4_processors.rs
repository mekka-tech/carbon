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
  std::{
      collections::{HashMap, HashSet},
      env,
      sync::Arc,
  },
  solana_transaction_status::TransactionTokenBalance,
  tokio::sync::RwLock,
  yellowstone_grpc_proto::geyser::{
      CommitmentLevel, SubscribeRequestFilterAccounts, SubscribeRequestFilterTransactions,
  },
  bullmq_rust::queue_service::QueueService,
  bullmq_rust::job_model::JobData,
  bullmq_rust::config_service::ConfigService,
  chrono::Utc,
  crate::events::events::{SummarizedTokenBalance, TokenTradeAnalysis, TradeType, ProtocolType},
  serde_json::Result,
};

fn summarize_token_balances(token_balances: &Vec<&TransactionTokenBalance>) -> Vec<SummarizedTokenBalance> {
  let mut mint_to_balance: HashMap<String, SummarizedTokenBalance> = HashMap::new();
  
  for tb in token_balances {
      let mint = tb.mint.clone();
      let entry = mint_to_balance.entry(mint.clone()).or_insert_with(|| SummarizedTokenBalance {
          mint: mint.clone(),
          amount: "0".to_string(),
          ui_amount: 0.0,
          decimals: tb.ui_token_amount.decimals,
          owner: tb.owner.clone(),
      });
      
      if let Ok(current_amount) = entry.amount.parse::<i128>() {
          if let Ok(tb_amount) = tb.ui_token_amount.amount.parse::<i128>() {
              entry.amount = (current_amount + tb_amount).to_string();
          }
      }
      
      entry.ui_amount += tb.ui_token_amount.ui_amount.unwrap_or(0.0);
  }
  
  mint_to_balance.values().cloned().collect()
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
        let tx_meta = &metadata.transaction_metadata.meta;
        
        match instruction.data {
            RaydiumAmmV4Instruction::Initialize2(init_pool) => match Initialize2::arrange_accounts(&accounts) {
                Some(accounts) => {
                    println!(
                            "AMM V4 Initialize2: signature: {signature}, init_pool: {init_pool:?}, accounts: {accounts:#?}",
                    );
                }
                None => println!("Failed to arrange accounts for AMM V4 Initialize2 {}", accounts.len()),
            },
            RaydiumAmmV4Instruction::SwapBaseIn(swap_base_in) => match SwapBaseIn::arrange_accounts(&accounts) {
                Some(accounts) => {
                    println!("AMM V4 SwapBaseIn: signature: {signature}, swap_base_in: {swap_base_in:?}, accounts: {accounts:#?}");

                    let user_account = accounts.user_source_owner.to_string().clone();
                    let post_token_balances = tx_meta.post_token_balances
                        .clone()
                        .unwrap_or_default();
                    let pre_token_balances = tx_meta.pre_token_balances.clone().unwrap_or_default();
                    
                    let user_pre_token_balance: Vec<_> = pre_token_balances
                        .iter()
                        .filter(|token_balance| token_balance.owner == user_account || (token_balance.owner == "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1" && token_balance.mint == "So11111111111111111111111111111111111111112"))
                        .collect();
                    let user_post_token_balance: Vec<_> = post_token_balances
                        .iter()
                        .filter(|token_balance| token_balance.owner == user_account || (token_balance.owner == "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1" && token_balance.mint == "So11111111111111111111111111111111111111112"))
                        .collect();
                    
                    let summarized_pre_balances = summarize_token_balances(&user_pre_token_balance);
                    let summarized_post_balances = summarize_token_balances(&user_post_token_balance);
                    
                    // let trade_analysis = analyze_token_trades(
                    //     &summarized_pre_balances, &summarized_post_balances, swap_base_in.amount_in);
                    // println!("[SwapBaseIn] Trade Analysis: {:#?}", trade_analysis);
                    
                    println!("[SwapBaseIn] Summarized pre token balances: {user_pre_token_balance:#?}");
                    println!("[SwapBaseIn] AmountIn: {:?}", swap_base_in.amount_in);
                    println!("[SwapBaseIn] Summarized post token balances: {user_post_token_balance:#?}");

                    // Optionally convert to JSON
                    // let json_output = serde_json::to_string_pretty(&trade_analysis).unwrap();
                    // println!("Trade Analysis JSON: {}", json_output);
                }
                None => println!("Failed to arrange accounts for AMM V4 SwapBaseIn {}", accounts.len()),
            },
            // RaydiumAmmV4Instruction::SwapBaseOut(swap_base_out) => match SwapBaseOut::arrange_accounts(&accounts) {
            //     Some(accounts) => {
            //         println!("AMM V4 SwapBaseOut: signature: {signature}, swap_base_out: {swap_base_out:?}, accounts: {accounts:#?}");

            //         let user_account = accounts.user_source_owner.to_string().clone();
            //         let post_token_balances = tx_meta.post_token_balances
            //             .clone()
            //             .unwrap_or_default();
            //         let pre_token_balances = tx_meta.pre_token_balances.clone().unwrap_or_default();

            //         let user_pre_token_balance: Vec<_> = pre_token_balances
            //             .iter()
            //             .filter(|token_balance| token_balance.owner == user_account)
            //             .collect();
            //         let user_post_token_balance: Vec<_> = post_token_balances
            //             .iter()
            //             .filter(|token_balance| token_balance.owner == user_account)
            //             .collect();

            //         // let summarized_pre_balances = summarize_token_balances(&user_pre_token_balance);
            //         // let summarized_post_balances = summarize_token_balances(&user_post_token_balance);
                    
            //         // let trade_analysis = analyze_token_trades(&summarized_pre_balances, &summarized_post_balances);
            //         // println!("[SwapBaseOut] Trade Analysis: {:#?}", trade_analysis);
            //         println!("[SwapBaseOut] Summarized pre token balances: {user_pre_token_balance:#?}");
            //         println!("[SwapBaseOut] AmountOut: {:?}", swap_base_out.amount_out);
            //         println!("[SwapBaseOut] Summarized post token balances: {user_post_token_balance:#?}");

            //     }
            //     None => println!("Failed to arrange accounts for AMM V4 SwapBaseOut {}", accounts.len()),
            // },
            _ => {
                // Ignored
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

// Improved function to analyze trades
fn analyze_token_trades(
    pre_balances: &[SummarizedTokenBalance], 
    post_balances: &[SummarizedTokenBalance],
    amount_in: u64,
) -> HashMap<String, TokenTradeAnalysis> {
    let mut result = HashMap::new();
    
    // Create maps for faster lookup
    let pre_map: HashMap<String, &SummarizedTokenBalance> = pre_balances
        .iter()
        .map(|balance| (balance.mint.clone(), balance))
        .collect();
    
    let post_map: HashMap<String, &SummarizedTokenBalance> = post_balances
        .iter()
        .map(|balance| (balance.mint.clone(), balance))
        .collect();
    
    // Find all unique mints
    let mut all_mints = HashSet::new();
    pre_balances.iter().for_each(|b| { all_mints.insert(b.mint.clone()); });
    post_balances.iter().for_each(|b| { all_mints.insert(b.mint.clone()); });
    
    // SOL token mint to ignore
    // const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
        
    // Analyze each mint
    for mint in all_mints {
        // Skip SOL token
        // if mint == SOL_MINT {
        //     continue;
        // }
        
        let pre_balance = pre_map.get(&mint).map(|b| b.ui_amount).unwrap_or(0.0);
        let post_balance = post_map.get(&mint).map(|b| b.ui_amount).unwrap_or(0.0);
        let amount_changed = post_balance - pre_balance;
        
        // Skip if no change
        if amount_changed.abs() < 0.000001 {
            continue;
        }
        
        let trade_type = if amount_changed > 0.0 {
            // If we received tokens, it's a buy
            TradeType::Buy
        } else {
            // If we spent tokens, it's a sell
            TradeType::Sell
        };
        
        // Determine if this is the token being swapped in
        let is_swap_in_token = amount_changed.abs() != amount_in as f64;
        
        // Get other details from either pre or post balance
        if let Some(details) = post_map.get(&mint).or_else(|| pre_map.get(&mint)) {
            result.insert(mint.clone(), TokenTradeAnalysis {
                mint: mint.clone(),
                trade_type,
                pre_balance,
                post_balance,
                amount_changed: amount_changed.abs(),
                decimals: details.decimals,
                owner: details.owner.clone(),
                protocol: ProtocolType::RaydiumAmmV4,
                amount_in,
                is_swap_in_token,
            });
        }
    }
    
    result
}

