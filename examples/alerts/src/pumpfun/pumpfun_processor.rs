use {
    crate::pumpfun::order_book::{ORDER_BOOK, Side},
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
    },
    carbon_pumpfun_decoder::instructions::{PumpfunInstruction, buy::Buy, sell::Sell},
    core::time,
    serde_json::Result,
    std::collections::HashMap,
    std::sync::Arc,
    chrono::{DateTime, Utc, TimeZone},
};
use tungstenite::{WebSocket, stream::MaybeTlsStream, Message};
use crate::pumpfun::swap::{SwapOrder, SOCKET};


fn time_ago(timestamp: i64) -> String {
    // Convert the Unix timestamp (in seconds) to a DateTime<Utc>.
    let purchase_time = Utc.timestamp(timestamp, 0);
    let now = Utc::now();
    let elapsed = now.signed_duration_since(purchase_time);

    format!("{} ms ago", elapsed.num_milliseconds())

}
const SOL_PRICE: f64 = 131.6;
// Define the list of valid pump user addresses.
const PUMP_USERS: &[&str] = &[
    "JDd3hy3gQn2V982mi1zqhNqUw1GfV2UL6g76STojCJPN",
    "DfMxre4cKmvogbLrPigxmibVTTQDuzjdXojWzjCXXhzj",
    "AJ6MGExeK7FXmeKkKPmALjcdXVStXYokYNv9uVfDRtvo",
    "CyaE1VxvBrahnPWkqm5VsdCvyS2QmNht2UFrKJHga54o",
    "DNfuF1L62WWyW3pNakVkyGGFzVVhj4Yr52jSmdTyeBHm",
    "BCnqsPEtA1TkgednYEebRpkmwFRJDCjMQcKZMMtEdArc",
    "73LnJ7G9ffBDjEBGgJDdgvLUhD5APLonKrNiHsKDCw5B",
    "5rkPDK4JnVAumgzeV2Zu8vjggMTtHdDtrsd5o9dhGZHD",
    "6m5sW6EAPAHncxnzapi1ZVJNRb9RZHQ3Bj7FD84X9rAF",
    "4BdKaxN8G6ka4GYtQQWk4G4dZRUTX2vQH9GcXdBREFUk",
    "BCagckXeMChUKrHEd6fKFA1uiWDtcmCXMsqaheLiUPJd",
    "3pZ59YENxDAcjaKa3sahZJBcgER4rGYi4v6BpPurmsGj",
    "EHg5YkU2SZBTvuT87rUsvxArGp3HLeye1fXaSDfuMyaf",
    "8rvAsDKeAcEjEkiZMug9k8v1y8mW6gQQiMobd89Uy7qR",
    "7iabBMwmSvS4CFPcjW2XYZY53bUCHzXjCFEFhxeYP4CY",
    "As7HjL7dzzvbRbaD3WCun47robib2kmAKRXMvjHkSMB5",
    "96sErVjEN7LNJ6Uvj63bdRWZxNuBngj56fnT9biHLKBf",
    "F72vY99ihQsYwqEDCfz7igKXA5me6vN2zqVsVUTpw6qL",
    "215nhcAHjQQGgwpQSJQ7zR26etbjjtVdW74NLzwEgQjP",
    "GJA1HEbxGnqBhBifH9uQauzXSB53to5rhDrzmKxhSU65",
    "G3g1CKqKWSVEVURZDNMazDBv7YAhMNTjhJBVRTiKZygk",
    "BXNiM7pqt9Ld3b2Hc8iT3mA5bSwoe9CRrtkSUs15SLWN",
    "7ABz8qEFZTHPkovMDsmQkm64DZWN5wRtU7LEtD2ShkQ6",
    "EaVboaPxFCYanjoNWdkxTbPvt57nhXGu5i6m9m6ZS2kK",
    "2YJbcB9G8wePrpVBcT31o8JEed6L3abgyCjt5qkJMymV",
    "DfMxre4cKmvogbLrPigxmibVTTQDuzjdXojWzjCXXhzj",
];

// const PUMP_USERS: &[&str] = &[
//     "DfMxre4cKmvogbLrPigxmibVTTQDuzjdXojWzjCXXhzj",
// ];


// const ORDER_BOOK: HashMap<String, f64> = HashMap::new();

pub struct PumpfunInstructionProcessor;


#[async_trait]
impl Processor for PumpfunInstructionProcessor {
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
        // let signature = metadata.transaction_metadata.signature;
        let accounts = instruction.accounts;
        
        match instruction.data {
            MoonshotInstruction::Buy(buy) => match Buy::arrange_accounts(&accounts) {
                Some(accounts) => {
                    log::info!(
                        "Buy: signature: {signature}, buy: {buy:?}, accounts: {accounts:#?}"
                    );
                }
                None => log::error!("Failed to arrange accounts for Buy {}", accounts.len()),
            },
            MoonshotInstruction::Sell(sell) => match Sell::arrange_accounts(&accounts) {
                Some(accounts) => {
                    log::info!(
                        "Sell: signature: {signature}, sell: {sell:?}, accounts: {accounts:#?}"
                    );
                }
                None => log::error!("Failed to arrange accounts for Sell {}", accounts.len()),
            },
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

                match order_book.get_position(trade_event.mint.to_string().as_str()) {
                    Some(position) => {
                        // Make sure that position.current_price is not zero to avoid division by zero.
                        if position.current_price != 0.0 {
                            let diff = token_price_usd - position.current_price;
                            let pct_diff = (diff / position.current_price) * 100.0;
                            // If you have a position quantity, you can also calculate total PNL.
                            // For example, if position has a `quantity` field:
                            let total_pnl = diff * position.quantity;
                            println!(
                                "[{}] Position Tracking - [{}] \nBought Price: ${:.6}, Current Price: ${:.6}, Diff: ${:.6} ({:.6}%), Possible PNL: ${:.6}",
                                metadata.transaction_metadata.slot,
                                position.user,
                                position.current_price,
                                token_price_usd,
                                diff,
                                pct_diff,
                                total_pnl
                            );
                        } else {
                            println!("Position Tracking: Bought Price is zero, cannot compute difference.");
                        }

                    }
                    None => {
                        // println!("No position tracking possible");
                    }
                }

                if PUMP_USERS.contains(&user_str.as_str()) {
                    let mut socket = SOCKET.lock().unwrap();
                    let body = serde_json::to_string(&SwapOrder {
                        mint: trade_event.mint.to_string(),
                        amount: token_amount.to_string(),
                        price: token_price_in_sol.to_string(),
                        bonding_curve: "".to_string(),
                        associated_bonding_curve: "".to_string(),
                        decimal: 6,
                        is_buy: trade_event.is_buy,
                    }).unwrap();
                    socket.socket.send(Message::Text(body.into())).unwrap_or(());

                    if trade_event.is_buy {
                        order_book.process_trade(user_str.as_str(), trade_event.mint.to_string().as_str(), Side::Buy, token_price_usd, token_amount);
                    } else {
                        let pnl = order_book.process_trade(user_str.as_str(), trade_event.mint.to_string().as_str(), Side::Sell, token_price_usd, token_amount);
                        println!("PNL: {}", pnl.unwrap_or(0.0));
                    }
                    println!("Trade occurred: {}", time_ago(trade_event.timestamp));
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
            _ => {
                // Ignored
                // println!("Ignored instruction: {:#?}", instruction);
            }
        };

        Ok(())
    }
}
