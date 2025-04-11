use {
    crate::orders::order_book::{ORDER_BOOK, Side},
    crate::orders::order_position::PositionAction,
    std::fmt::Display,
    crate::events::{
        events::{ProtocolType, SummarizedTokenBalance, SwapResult, SwapType},
        rabbit::RabbitMQPublisher,
    },
    async_trait::async_trait,
    carbon_core::{
        error::CarbonResult,
        instruction::{DecodedInstruction, InstructionMetadata, NestedInstruction},
        metrics::MetricsCollection,
        processor::Processor,
        deserialize::ArrangeAccounts,
    },
    carbon_pumpfun_decoder::instructions::{PumpfunInstruction, buy::Buy, sell::Sell},
    core::time,
    serde_json::Result,
    std::collections::HashMap,
    std::sync::{Arc, Mutex},
    chrono::{DateTime, Utc, TimeZone},
};
use carbon_pumpfun_decoder::instructions::create::Create;
use tungstenite::{WebSocket, stream::MaybeTlsStream, Message};
use crate::pumpfun::swap::{SwapOrder, SOCKET};
use dotenv::dotenv;
use std::env;
use lazy_static::lazy_static;


fn time_ago(timestamp: i64) -> String {
    // Convert the Unix timestamp (in seconds) to a DateTime<Utc>.
    let purchase_time = Utc.timestamp(timestamp, 0);
    let now = Utc::now();
    let elapsed = now.signed_duration_since(purchase_time);

    format!("{} ms ago", elapsed.num_milliseconds())

}

const SOL_PRICE: f64 = 115.99;


lazy_static! {
    static ref OUR_WALLETS: Vec<String> = {
        dotenv().ok();
        vec![env::var("OWNER_ADDRESS").expect("OWNER_ADDRESS is empty in .env")]
    };
    static ref MAX_TOKEN_BUY: i32 = {
        dotenv().ok();
        env::var("MAX_TOKEN_BUY")
            .unwrap_or_else(|_| "1".to_string())
            .parse::<i32>()
            .unwrap_or(1)
    };
    static ref MIN_CREATOR_BALANCE: u64 = {
        dotenv().ok();
        env::var("MIN_CREATOR_BALANCE")
            .unwrap_or_else(|_| "1".to_string())
            .parse::<u64>()
            .unwrap_or(1) * 1_000_000_000
    };
    static ref MAX_CREATOR_BUY: u64 = {
        dotenv().ok();
        env::var("MAX_CREATOR_BUY")
            .unwrap_or_else(|_| "1".to_string())
            .parse::<u64>()
            .unwrap_or(1) * 1_000_000_000
    };
}

const VIRTUAL_SOL_RESERVES: u64 = 30000000017;
const VIRTUAL_TOKEN_RESERVES: u64 = 1073000000000000;
static COUNTER: Mutex<i32> = Mutex::new(0);

pub struct PumpfunNewTokensInstructionProcessor;

#[async_trait]
impl Processor for PumpfunNewTokensInstructionProcessor {
    type InputType = (
        InstructionMetadata,
        DecodedInstruction<PumpfunInstruction>,
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
            PumpfunInstruction::Create(create) => match Create::arrange_accounts(&accounts) {
                Some(accounts) => {
                    let pre_balance = metadata.transaction_metadata.meta.pre_balances[0];
                    let post_balance = metadata.transaction_metadata.meta.post_balances[0];

                    if post_balance > pre_balance {
                        return Ok(());
                    }
                    let diff_balance = pre_balance - post_balance;


            
                    if pre_balance < *MIN_CREATOR_BALANCE && diff_balance > *MAX_CREATOR_BUY {
                        return Ok(());
                    }
                    let mut counter = COUNTER.lock().unwrap();
                    if *counter > *MAX_TOKEN_BUY { return Ok(()); }
                    
                    println!("Pre Balance: {}", pre_balance);
                    println!("Post Balance: {}", post_balance);
                    println!("Diff Balance: {}", diff_balance);
                    println!("Min Creator Balance: {}", *MIN_CREATOR_BALANCE);
                    println!("Max Creator Buy: {}", *MAX_CREATOR_BUY);
                    
                    println!("Create Event: {:#?}", accounts);
                    let user_str = metadata.transaction_metadata.fee_payer.to_string();
                    let sol_amount: f64 = VIRTUAL_SOL_RESERVES as f64 / 1e9;
                    let token_amount: f64 = VIRTUAL_TOKEN_RESERVES as f64 / 1e6;
                    let timestamp = metadata.transaction_metadata.block_time.unwrap_or(Utc::now().timestamp_millis());
                    let mut socket = SOCKET.lock().unwrap();
                    let body = serde_json::to_string(&SwapOrder {
                        creator: user_str.to_string(),
                        mint: accounts.mint.to_string(),
                        amount: token_amount.to_string(),
                        sol_amount: sol_amount.to_string(),
                        bonding_curve: accounts.bonding_curve.to_string(),
                        associated_bonding_curve: accounts.associated_bonding_curve.to_string(),
                        decimals: 6,
                        is_buy: true,
                        origin: "normal".to_string(),
                        timestamp: timestamp,
                        signature: signature.to_string(),
                    }).unwrap();
                    socket.socket.send(Message::Text(body.into())).unwrap_or(());
                
                }
                None => log::error!("Failed to arrange accounts for Create {}", accounts.len()),
            },
            PumpfunInstruction::Buy(_) => {},
            PumpfunInstruction::Sell(_) => {},
            PumpfunInstruction::Initialize(_) => {},
            PumpfunInstruction::SetParams(_) => {},
            PumpfunInstruction::Withdraw(_) => {},
            PumpfunInstruction::CompleteEvent(_) => {},
            PumpfunInstruction::CreateEvent(_) => {},
            PumpfunInstruction::SetParamsEvent(_) => {},
            PumpfunInstruction::TradeEvent(trade_event) => {
                let user_str = trade_event.user.to_string();
                // Normalize the raw amounts.
                let sol_amount: f64 = trade_event.sol_amount as f64 / 1e9;
                let token_amount: f64 = trade_event.token_amount as f64 / 1e6;
                // Compute the token price in SOL. This tells you how many SOL one token costs.
                let token_price_in_sol: f64 = sol_amount / token_amount;
                // Convert token price to USD.
                let token_price_usd: f64 = token_price_in_sol * SOL_PRICE;

                let total_supply: u64 = 1_000_000_000; // For example.
                // Then compute the market cap in USD.
                let market_cap: f64 = token_price_usd * total_supply as f64;

                let mut order_book = ORDER_BOOK.lock().unwrap();

                match order_book.get_position_by_mint(trade_event.mint.to_string().as_str()) {
                    Some(position) => {
                        // Make sure that position.current_price is not zero to avoid division by zero.
                    if position.current_price != 0.0 {
                        let diff = token_price_usd - position.current_price;
                        let pct_diff = (diff / position.current_price) * 100.0;
                        // If you have a position quantity, you can also calculate total PNL.
                        // For example, if position has a `quantity` field:
                        let total_pnl = diff * position.quantity;

                        

                        let position_action = position.enhanced_position.process_price_update(token_price_usd);
                        if (position_action == PositionAction::EXIT) {
                            println!("MESSAGE SENT TO SOCKET");
                            let mut socket = SOCKET.lock().unwrap();
                            let body = serde_json::to_string(&SwapOrder {
                                creator: user_str.to_string(),
                                mint: trade_event.mint.to_string(),
                                amount: token_amount.to_string(),
                                sol_amount: sol_amount.to_string(),
                                bonding_curve: "".to_string(),
                                associated_bonding_curve: "".to_string(),
                                decimals: 6,
                                is_buy: false,
                                origin: "take_profit".to_string(),
                                timestamp: trade_event.timestamp,
                                signature: signature.to_string(),
                            }).unwrap();
                            socket.socket.send(Message::Text(body.into())).unwrap_or(());
                        }
                        println!(
                            "=========================\n[{}] Position Tracking - [{}] \n\nEntry${:.6} | Range: ${:.6} | Current: ${:.6}, ({:.6}%)\nAction: {} | PNL: ${:.6}\n================================================",
                            metadata.transaction_metadata.slot,
                            position.user,
                            position.enhanced_position.entry_price,
                            position.enhanced_position.actual_price,
                            token_price_usd,
                            pct_diff,
                            position_action,
                            total_pnl
                        );
                        println!("Position Action: {}", position_action);
                    } else {
                        println!("Position Tracking: Bought Price is zero, cannot compute difference.");
                    }

                }
                None => {
                    // println!("No position tracking possible");
                }
                }

                if OUR_WALLETS.contains(&user_str) {
                    let mut counter = COUNTER.lock().unwrap();  
                    if trade_event.is_buy {
                        order_book.process_trade(user_str.as_str(), trade_event.mint.to_string().as_str(), Side::Buy, token_price_usd, token_amount);
                        *counter += 1;
                    } else {
                        order_book.process_trade(user_str.as_str(), trade_event.mint.to_string().as_str(), Side::Sell, token_price_usd, token_amount);  
                        *counter -= 1;
                    }
                
                    println!("User: {}", user_str);
                    println!("Token Address: {}", trade_event.mint);
                    println!("Is Buy: {}", trade_event.is_buy);
                    println!("Token Amount: {}", trade_event.token_amount);
                    println!("Sol Amount: {}", trade_event.sol_amount);
                    println!("Token Price (USD): {}", token_price_usd);
                    println!("Market Cap (USD): {}", market_cap);

                    println!("Hash: https://solscan.io/tx/{}", metadata.transaction_metadata.signature);
                    println!("--------------------------------");
                }
            }
            // PumpfunInstruction::CompleteEvent(complete_event) => {
            //     println!("\nBonded: {:#?}", complete_event);
            // }
            // _ => {
            //     //Ignored
            //     println!("Ignored instruction: {:#?}", instruction);
            // }
        };

        Ok(())
    }
}
