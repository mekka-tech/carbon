use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TradeType {
    Buy,
    Sell,
    Unknown
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProtocolType {
    RaydiumAmmV4,
    RaydiumCpmm,
    RaydiumClmm,
    Pumpfun,
    Moonshot,
    Jupiter,
    Unknown
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenTradeAnalysis {
    pub mint: String,
    pub trade_type: TradeType,
    pub pre_balance: f64,
    pub post_balance: f64,
    pub amount_changed: f64,
    pub decimals: u8,
    pub owner: String,
    pub protocol: ProtocolType,
    pub amount_in: u64,
    pub is_swap_in_token: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizedTokenBalance {
    pub mint: String,
    pub amount: String,
    pub ui_amount: f64,
    pub decimals: u8,
    pub owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewPoolEventPayload {
    pub lp_mint: String,
    pub mint: String,
    pub owner: String,
    pub protocol: ProtocolType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTokenEventPayload {
    pub mint: String,
    pub owner: String,
}