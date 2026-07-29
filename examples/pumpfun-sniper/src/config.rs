use {
    crate::sender::fast::FastProvider,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    std::{
        collections::{BTreeSet, HashSet},
        env,
        str::FromStr,
    },
};

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SendPath {
    /// Independent `sendTransaction` to every RPC in the pool, skip preflight.
    Rpc,
    /// Anti-MEV / low-latency providers (Helius Sender, Jito sendTransaction,
    /// Nextblock, 0slot, …) configured via `FAST_PROVIDERS`.
    Fast,
    /// Direct QUIC to upcoming leaders' TPU, bypassing RPC entirely. See
    /// `sender/tpu.rs` — connections to the next `TPU_LEADERS_AHEAD` leaders
    /// are kept pre-warmed so a send is one round trip.
    Tpu,
    /// Parallel Jito bundles of 5 with tips (optional, off by default).
    Jito,
}

/// One buyer wallet plus its per-wallet dispatch parameters.
pub struct Buyer {
    pub keypair: Keypair,
    pub buy_amount_lamports: u64,
    pub priority_fee_micro_lamports: u64,
    /// Index into `Config::fast_providers`: which fast/anti-MEV endpoint this
    /// wallet's transaction is built and submitted for (its tip is baked in at
    /// build time). `None` means plain RPC spray only, no tip.
    pub provider: Option<usize>,
}

pub struct Config {
    pub geyser_url: String,
    pub x_token: Option<String>,
    pub rpc_urls: Vec<String>,

    pub watched_creators: HashSet<Pubkey>,
    pub blacklisted_creators: HashSet<Pubkey>,

    pub buyers: Vec<Buyer>,
    pub slippage_bps: u64,
    pub track_volume: bool,

    pub min_creator_balance_lamports: u64,
    pub max_creator_buy_lamports: u64,
    pub max_positions: usize,
    pub max_tx_age_ms: i64,

    /// SEND_MODE=simulate: build + simulate against the first RPC, never send.
    pub dry_run: bool,
    pub send_paths: BTreeSet<SendPath>,
    pub fast_providers: Vec<FastProvider>,
    pub compute_unit_limit: u32,
    pub jito_block_engine_urls: Vec<String>,
    pub jito_tip_lamports: u64,
    /// How many distinct upcoming leaders the direct-TPU path targets. 0
    /// disables the path even if `SEND_PATHS` lists `tpu`.
    pub tpu_leaders_ahead: u64,

    /// Extra lamports (on top of buy amount) each wallet must hold to cover
    /// fees, ATA rent, and first-buy account rents. Preflight uses this.
    pub funding_buffer_lamports: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let geyser_url = require("GEYSER_URL")?;
        let rpc_urls = parse_csv(&require("RPC_URLS")?);
        if rpc_urls.is_empty() {
            return Err("RPC_URLS must list at least one endpoint".into());
        }

        let watched_creators =
            parse_pubkey_list(&env::var("WATCHED_CREATORS").unwrap_or_default())?;
        if watched_creators.is_empty() {
            return Err("WATCHED_CREATORS must list at least one creator wallet".into());
        }
        let blacklisted_creators =
            parse_pubkey_list(&env::var("BLACKLISTED_CREATORS").unwrap_or_default())?;

        let keypairs = load_keypairs()?;
        if keypairs.is_empty() {
            return Err("no buyer keypairs loaded".into());
        }

        let dry_run = matches!(
            env::var("SEND_MODE").unwrap_or_default().as_str(),
            "" | "simulate"
        );
        let send_paths =
            parse_send_paths(&env::var("SEND_PATHS").unwrap_or_else(|_| "rpc".into()))?;
        let fast_providers = parse_fast_providers(&env::var("FAST_PROVIDERS").unwrap_or_default())?;
        if send_paths.contains(&SendPath::Fast) && fast_providers.is_empty() {
            return Err("SEND_PATHS includes 'fast' but FAST_PROVIDERS is empty".into());
        }

        let default_buy = sol_env("BUY_AMOUNT_SOL", 0.01)?;
        let base_fee = num_env("PRIORITY_FEE_MICRO_LAMPORTS", 100_000)?;
        // Spread priority fees across [base, base + jitter] deterministically by
        // wallet index, so the 30 buys occupy a range of priority levels rather
        // than tying at one price.
        let jitter = num_env("PRIORITY_FEE_JITTER", 0)?;
        let n = keypairs.len() as u64;
        // Wallets are dealt round-robin across the configured fast providers,
        // so the 30 buys arrive at the leader over several independent routes.
        let use_fast = send_paths.contains(&SendPath::Fast) && !fast_providers.is_empty();
        let buyers = keypairs
            .into_iter()
            .enumerate()
            .map(|(i, keypair)| Buyer {
                keypair,
                buy_amount_lamports: default_buy,
                priority_fee_micro_lamports: if n > 1 {
                    base_fee + jitter * i as u64 / (n - 1)
                } else {
                    base_fee
                },
                provider: use_fast.then(|| i % fast_providers.len()),
            })
            .collect();

        Ok(Self {
            geyser_url,
            x_token: env::var("X_TOKEN").ok(),
            rpc_urls,
            watched_creators,
            blacklisted_creators,
            buyers,
            slippage_bps: num_env("SLIPPAGE_BPS", 500)?,
            track_volume: env::var("TRACK_VOLUME").is_ok_and(|v| v == "true"),
            min_creator_balance_lamports: sol_env("MIN_CREATOR_BALANCE_SOL", 0.0)?,
            max_creator_buy_lamports: sol_env("MAX_CREATOR_BUY_SOL", 5.0)?,
            max_positions: num_env("MAX_POSITIONS", 5)? as usize,
            max_tx_age_ms: num_env("MAX_TX_AGE_MS", 3_000)? as i64,
            dry_run,
            send_paths,
            fast_providers,
            compute_unit_limit: num_env("COMPUTE_UNIT_LIMIT", 120_000)? as u32,
            // Jito allows 1 request/s per region per IP, so bundles are dealt
            // across regions. Nearest-first.
            jito_block_engine_urls: parse_csv(&env::var("JITO_BLOCK_ENGINE_URLS").unwrap_or_else(
                |_| {
                    "https://frankfurt.mainnet.block-engine.jito.wtf,\
                     https://amsterdam.mainnet.block-engine.jito.wtf,\
                     https://london.mainnet.block-engine.jito.wtf,\
                     https://dublin.mainnet.block-engine.jito.wtf,\
                     https://ny.mainnet.block-engine.jito.wtf,\
                     https://slc.mainnet.block-engine.jito.wtf"
                        .to_string()
                },
            )),
            jito_tip_lamports: sol_env("JITO_TIP_SOL", 0.0001)?,
            tpu_leaders_ahead: num_env("TPU_LEADERS_AHEAD", 2)?,
            funding_buffer_lamports: sol_env("FUNDING_BUFFER_SOL", 0.01)?,
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

fn parse_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn parse_send_paths(raw: &str) -> Result<BTreeSet<SendPath>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| match s {
            "rpc" => Ok(SendPath::Rpc),
            "fast" => Ok(SendPath::Fast),
            "tpu" => Ok(SendPath::Tpu),
            "jito" => Ok(SendPath::Jito),
            other => Err(format!(
                "SEND_PATHS entry must be rpc|fast|tpu|jito, got {other}"
            )),
        })
        .collect()
}

/// Fast/anti-MEV providers, one per line:
/// `name|url|tip_account[+tip_account...]|tip_sol[|auth_header[|format]]`
///
/// Tip accounts, minimum tips, and request-body format differ per provider —
/// take them from that provider's own documentation rather than assuming. A
/// provider that needs no tip takes an empty tip-account field and 0 tip.
fn parse_fast_providers(raw: &str) -> Result<Vec<FastProvider>, String> {
    raw.split([';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|line| {
            let parts: Vec<&str> = line.split('|').map(str::trim).collect();
            if parts.len() < 4 {
                return Err(format!(
                    "FAST_PROVIDERS entry needs name|url|tip_accounts|tip_sol, got: {line}"
                ));
            }
            let tip_accounts = parts[2]
                .split('+')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Pubkey::from_str(s).map_err(|_| format!("invalid tip account: {s}")))
                .collect::<Result<Vec<_>, _>>()?;
            let tip_sol: f64 = parts[3]
                .parse()
                .map_err(|_| format!("invalid tip amount (SOL): {}", parts[3]))?;
            Ok(FastProvider {
                name: parts[0].to_string(),
                url: parts[1].to_string(),
                tip_accounts,
                tip_lamports: (tip_sol * LAMPORTS_PER_SOL) as u64,
                auth_header: parts
                    .get(4)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
                format: parts.get(5).unwrap_or(&"").parse()?,
            })
        })
        .collect()
}

fn parse_pubkey_list(raw: &str) -> Result<HashSet<Pubkey>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Pubkey::from_str(s).map_err(|_| format!("invalid pubkey: {s}")))
        .collect()
}

/// Buyer keypairs come from either `BUYER_KEYPAIR_DIR` (a directory of
/// solana-keygen JSON files, the practical option for 30 wallets, loaded in
/// sorted filename order) or `BUYER_KEYPAIRS` (comma/newline-separated base58
/// secret keys).
fn load_keypairs() -> Result<Vec<Keypair>, String> {
    if let Ok(dir) = env::var("BUYER_KEYPAIR_DIR") {
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| format!("cannot read BUYER_KEYPAIR_DIR {dir}: {e}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        return paths
            .iter()
            .map(|path| {
                let raw = std::fs::read_to_string(path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                let bytes: Vec<u8> = serde_json::from_str(raw.trim())
                    .map_err(|e| format!("invalid keypair JSON in {}: {e}", path.display()))?;
                Keypair::try_from(bytes.as_slice())
                    .map_err(|e| format!("invalid keypair in {}: {e}", path.display()))
            })
            .collect();
    }

    require("BUYER_KEYPAIRS")?
        .split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|raw| {
            let bytes = bs58::decode(raw)
                .into_vec()
                .map_err(|e| format!("invalid base58 keypair: {e}"))?;
            Keypair::try_from(bytes.as_slice()).map_err(|e| format!("invalid keypair bytes: {e}"))
        })
        .collect()
}
