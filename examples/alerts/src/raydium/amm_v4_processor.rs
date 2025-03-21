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
    carbon_raydium_amm_v4_decoder::{
        accounts::RaydiumAmmV4Account,
        instructions::{
            initialize2::Initialize2, swap_base::SwapBaseInstructionAccounts,
            swap_base_in::SwapBaseIn, swap_base_out::SwapBaseOut, RaydiumAmmV4Instruction,
        },
        RaydiumAmmV4Decoder,
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

const RAYDIUM_AUTHORITY: &str = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1";
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

fn summarize_token_balances(
    token_balances: &Vec<&TransactionTokenBalance>,
) -> Vec<SummarizedTokenBalance> {
    let mut mint_to_balance: HashMap<String, SummarizedTokenBalance> = HashMap::new();

    for tb in token_balances {
        let mint = tb.mint.clone();
        let entry = mint_to_balance
            .entry(mint.clone())
            .or_insert_with(|| SummarizedTokenBalance {
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

fn process_swap(
    accounts: &SwapBaseInstructionAccounts,
    tx_meta: &TransactionStatusMeta,
    signature: &str,
    block_time: &i64,
) -> Option<SwapResult> {
    let user_account = accounts.user_source_owner.to_string().clone();
    let post_token_balances = tx_meta.post_token_balances.clone().unwrap_or_default();
    let pre_token_balances = tx_meta.pre_token_balances.clone().unwrap_or_default();

    let user_pre_token_balance: Vec<_> = pre_token_balances
        .iter()
        .filter(|token_balance| {
            token_balance.owner == user_account
                || (token_balance.owner == RAYDIUM_AUTHORITY && token_balance.mint == WSOL_MINT)
        })
        .collect();
    let user_post_token_balance: Vec<_> = post_token_balances
        .iter()
        .filter(|token_balance| {
            token_balance.owner == user_account
                || (token_balance.owner == RAYDIUM_AUTHORITY && token_balance.mint == WSOL_MINT)
        })
        .collect();

    let summarized_pre_balances = summarize_token_balances(&user_pre_token_balance);
    let summarized_post_balances = summarize_token_balances(&user_post_token_balance);

    // let trade_analysis = analyze_token_trades(
    //     &summarized_pre_balances, &summarized_post_balances, swap_base_in.amount_in);
    // println!("[SwapBaseIn] Trade Analysis: {:#?}", trade_analysis);

    // println!("[SwapBaseIn] Summarized pre token balances: {summarized_pre_balances:#?}");
    // println!("[SwapBaseIn] AmountIn: {:?}", swap_base_in.amount_in);
    // println!("[SwapBaseIn] Summarized post token balances: {summarized_post_balances:#?}");

    analyze_swap(
        &summarized_pre_balances,
        &summarized_post_balances,
        &signature,
        accounts.amm.to_string().as_str(),
        block_time,
    )
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
        let signature = metadata.transaction_metadata.signature.to_string();
        // let block_time = metadata.transaction_metadata.block_time;
        let accounts = instruction.accounts;
        // let tx_meta = &metadata.transaction_metadata.meta;

        match instruction.data {
            RaydiumAmmV4Instruction::Initialize2(init_pool) => {
                match Initialize2::arrange_accounts(&accounts) {
                    Some(accounts) => {
                        println!(
                            "AMM V4 Initialize2: signature: {signature}, init_pool: {init_pool:?}, accounts: {accounts:#?}",
                    );
                    }
                    None => println!(
                        "Failed to arrange accounts for AMM V4 Initialize2 {}",
                        accounts.len()
                    ),
                }
            }
            // RaydiumAmmV4Instruction::SwapBaseIn(swap_base_in) => match SwapBaseIn::arrange_accounts(&accounts) {
            //     Some(accounts) => {
            //         // println!("AMM V4 SwapBaseIn: signature: {signature}, swap_base_in: {swap_base_in:?}, accounts: {accounts:#?}");

            //         let swap_result = process_swap(&accounts, &tx_meta, &signature, &block_time.unwrap_or(0));
            //         match swap_result {
            //             Some(swap_result) => {
            //                 // Optionally convert to JSON
            //                 let json_output = serde_json::to_string_pretty(&swap_result).unwrap();
            //                 // println!("Swap Result JSON: {}", json_output);

            //                 RabbitMQPublisher::publish_swap_result(&swap_result).await?;
            //             }
            //             None => println!("Failed to process swap for AMM V4 SwapBaseIn"),
            //         }
            //     }
            //     None => println!("Failed to arrange accounts for AMM V4 SwapBaseIn {}", accounts.len()),
            // },
            // RaydiumAmmV4Instruction::SwapBaseOut(swap_base_out) => match SwapBaseOut::arrange_accounts(&accounts) {
            //     Some(accounts) => {
            //         // println!("AMM V4 SwapBaseOut: signature: {signature}, swap_base_out: {swap_base_out:?}, accounts: {accounts:#?}");

            //         let swap_result = process_swap(&accounts, &tx_meta, &signature, &block_time.unwrap_or(0));
            //         match swap_result {
            //             Some(swap_result) => {
            //                 // Optionally convert to JSON
            //                 let json_output = serde_json::to_string_pretty(&swap_result).unwrap();
            //                 // println!("Swap Result JSON: {}", json_output);

            //                 RabbitMQPublisher::publish_swap_result(&swap_result).await?;
            //             }
            //             None => println!("Failed to process swap for AMM V4 SwapBaseIn"),
            //         }
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

/// Analyzes pre and post swap token balances and determines if the operation
/// was a buy or sell, the amounts involved, and the effective price.
///
/// # Assumptions
/// - The WSOL token is identified by its owner being "raydium_authority".
/// - The other token is owned by the swap user.
/// - A positive difference in the raydium authority WSOL account indicates a buy
///   (you sent WSOL to raydium), while a negative difference indicates a sell.
///
/// # Returns
/// - `Some(SwapResult)` if the analysis is successful.
/// - `None` if the required balances cannot be found or if an inconsistency occurs.
pub fn analyze_swap(
    pre_balances: &[SummarizedTokenBalance],
    post_balances: &[SummarizedTokenBalance],
    tx_hash: &str,
    pool: &str,
    block_time: &i64,
) -> Option<SwapResult> {
    // Find the WSOL account (owned by raydium authority) in pre and post balances
    let pre_wsol = pre_balances
        .iter()
        .find(|b| b.owner == RAYDIUM_AUTHORITY && b.mint == WSOL_MINT)?;
    let post_wsol = post_balances
        .iter()
        .find(|b| b.owner == RAYDIUM_AUTHORITY && b.mint == WSOL_MINT)?;

    // Find the token account (owned by the swap user) in pre and post balances
    let default_token = SummarizedTokenBalance {
        mint: "".to_string(),
        amount: "0".to_string(),
        ui_amount: 0.0,
        decimals: 0,
        owner: "".to_string(),
    };

    let pre_token = pre_balances
        .iter()
        .find(|b| b.owner != RAYDIUM_AUTHORITY && b.mint != WSOL_MINT)
        .unwrap_or(&default_token);

    let post_token = post_balances
        .iter()
        .find(|b| b.owner != RAYDIUM_AUTHORITY && b.mint != WSOL_MINT)
        .unwrap_or(&default_token);

    // Compute the differences in WSOL and token balances
    let wsol_diff = post_wsol.ui_amount - pre_wsol.ui_amount;
    let token_diff = post_token.ui_amount - pre_token.ui_amount;

    // Determine swap type:
    // If WSOL balance increases on the raydium account, you've sent WSOL (buy).
    // Otherwise, if it decreases, you received WSOL (sell).
    let swap_type = if wsol_diff > 0.0 {
        SwapType::Buy
    } else if wsol_diff < 0.0 {
        SwapType::Sell
    } else {
        // If there is no change in WSOL, we cannot determine the swap type.
        SwapType::Unknown
    };

    // Use absolute values for amounts.
    let wsol_amount = wsol_diff.abs();
    let token_amount = token_diff.abs();

    let mut price = 0.0;
    // Avoid division by zero
    if token_amount != 0.0 {
        price = wsol_amount / token_amount;
    }

    let sol_price = 125.64;
    let price_usd = price * sol_price;

    Some(SwapResult {
        swap_type,
        token_amount: token_amount.to_string(),
        wsol_amount: wsol_amount.to_string(),
        price: price.to_string(),
        price_usd: price_usd.to_string(),
        total_usd: (price_usd * token_amount).to_string(),
        is_swandwich_attack: price == 0.0,
        tx_hash: tx_hash.to_string(),
        user: pre_token.owner.to_string(),
        mint: pre_token.mint.to_string(),
        pool: pool.to_string(),
        protocol: ProtocolType::RaydiumAmmV4,
        timestamp: block_time.clone(),
    })
}
