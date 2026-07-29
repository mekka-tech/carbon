use {
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    std::{collections::HashSet, env, str::FromStr},
};

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    /// Build + simulate against RPC, never send. Default.
    Simulate,
    /// Independent `sendTransaction` per buyer, skip_preflight.
    RpcSpray,
    /// All buyer txs in a single Jito bundle (tip on the last tx).
    JitoBundle,
}

pub struct Config {
    pub geyser_url: String,
    pub x_token: Option<String>,
    pub rpc_url: String,

    pub watched_creators: HashSet<Pubkey>,
    pub blacklisted_creators: HashSet<Pubkey>,

    pub buyers: Vec<Keypair>,
    pub buy_amount_lamports: u64,
    pub slippage_bps: u64,
    pub track_volume: bool,

    pub min_creator_balance_lamports: u64,
    pub max_creator_buy_lamports: u64,
    pub max_positions: usize,
    pub max_tx_age_ms: i64,

    pub send_mode: SendMode,
    pub compute_unit_limit: u32,
    pub priority_fee_micro_lamports: u64,
    pub jito_block_engine_url: String,
    pub jito_tip_lamports: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let geyser_url = require("GEYSER_URL")?;
        let rpc_url = require("RPC_URL")?;

        let watched_creators =
            parse_pubkey_list(&env::var("WATCHED_CREATORS").unwrap_or_default())?;
        if watched_creators.is_empty() {
            return Err("WATCHED_CREATORS must list at least one creator wallet".into());
        }
        let blacklisted_creators =
            parse_pubkey_list(&env::var("BLACKLISTED_CREATORS").unwrap_or_default())?;

        let buyers = parse_keypairs(&require("BUYER_KEYPAIRS")?)?;
        if buyers.is_empty() {
            return Err("BUYER_KEYPAIRS must contain at least one keypair".into());
        }

        let send_mode = match env::var("SEND_MODE").unwrap_or_default().as_str() {
            "" | "simulate" => SendMode::Simulate,
            "rpc" => SendMode::RpcSpray,
            "jito" => SendMode::JitoBundle,
            other => return Err(format!("SEND_MODE must be simulate|rpc|jito, got {other}")),
        };

        Ok(Self {
            geyser_url,
            x_token: env::var("X_TOKEN").ok(),
            rpc_url,
            watched_creators,
            blacklisted_creators,
            buyers,
            buy_amount_lamports: sol_env("BUY_AMOUNT_SOL", 0.01)?,
            slippage_bps: num_env("SLIPPAGE_BPS", 500)?,
            track_volume: env::var("TRACK_VOLUME").is_ok_and(|v| v == "true"),
            min_creator_balance_lamports: sol_env("MIN_CREATOR_BALANCE_SOL", 0.0)?,
            max_creator_buy_lamports: sol_env("MAX_CREATOR_BUY_SOL", 5.0)?,
            max_positions: num_env("MAX_POSITIONS", 5)? as usize,
            max_tx_age_ms: num_env("MAX_TX_AGE_MS", 3_000)? as i64,
            send_mode,
            compute_unit_limit: num_env("COMPUTE_UNIT_LIMIT", 120_000)? as u32,
            priority_fee_micro_lamports: num_env("PRIORITY_FEE_MICRO_LAMPORTS", 100_000)?,
            jito_block_engine_url: env::var("JITO_BLOCK_ENGINE_URL")
                .unwrap_or_else(|_| "https://mainnet.block-engine.jito.wtf".to_string()),
            jito_tip_lamports: sol_env("JITO_TIP_SOL", 0.0001)?,
        })
    }
}

fn require(key: &str) -> Result<String, String> {
    env::var(key).map_err(|_| format!("{key} must be set"))
}

fn num_env(key: &str, default: u64) -> Result<u64, String> {
    match env::var(key) {
        Ok(v) => v.parse().map_err(|_| format!("{key} must be a number")),
        Err(_) => Ok(default),
    }
}

fn sol_env(key: &str, default_sol: f64) -> Result<u64, String> {
    let sol = match env::var(key) {
        Ok(v) => v
            .parse::<f64>()
            .map_err(|_| format!("{key} must be a number (SOL)"))?,
        Err(_) => default_sol,
    };
    Ok((sol * LAMPORTS_PER_SOL) as u64)
}

fn parse_pubkey_list(raw: &str) -> Result<HashSet<Pubkey>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Pubkey::from_str(s).map_err(|_| format!("invalid pubkey: {s}")))
        .collect()
}

/// Comma-separated keypairs, each either a base58-encoded 64-byte secret key
/// or a JSON byte array (solana-keygen file contents).
fn parse_keypairs(raw: &str) -> Result<Vec<Keypair>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_keypair)
        .collect()
}

fn parse_keypair(raw: &str) -> Result<Keypair, String> {
    let bytes: Vec<u8> = if raw.starts_with('[') {
        serde_json::from_str(raw).map_err(|e| format!("invalid keypair JSON: {e}"))?
    } else {
        bs58::decode(raw)
            .into_vec()
            .map_err(|e| format!("invalid base58 keypair: {e}"))?
    };
    Keypair::try_from(bytes.as_slice()).map_err(|e| format!("invalid keypair bytes: {e}"))
}
