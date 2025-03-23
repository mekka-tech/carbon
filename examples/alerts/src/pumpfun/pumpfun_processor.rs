use {
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
    carbon_pumpfun_decoder::instructions::PumpfunInstruction,
    core::time,
    serde_json::Result,
    std::sync::Arc,
    chrono::{DateTime, Utc, TimeZone},
};

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
];


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
        // let accounts = instruction.accounts;

        match instruction.data {
            // PumpfunInstruction::CreateEvent(create_event) => {
            //     println!("\nNew token created: {:#?}", create_event);
            // }
            PumpfunInstruction::TradeEvent(trade_event) => {
                let user_str = trade_event.user.to_string();
                if PUMP_USERS.contains(&user_str.as_str()) {
                    println!("Trade occurred: {}", time_ago(trade_event.timestamp));
                    println!("User: {}", user_str);
                    println!("Token Address: {}", trade_event.mint);
                    println!("Is Buy: {}", trade_event.is_buy);
                    println!("Token Amount: {}", trade_event.token_amount);
                    println!("Sol Amount: {}", trade_event.sol_amount);
                    let token_price = trade_event.sol_amount / trade_event.token_amount;
                    let token_price_usd = token_price as f64 * SOL_PRICE;
                    println!("Token Price: {}", token_price_usd);
                    println!("Market Cap: {}", token_price_usd * 1000000000);

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
