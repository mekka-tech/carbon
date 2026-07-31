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

/// Which feed creates are detected on. They differ in more than transport — see
/// `Config::synthetic_meta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasourceKind {
    /// Yellowstone geyser at `Processed`. Carries real `TransactionStatusMeta`
    /// and real `block_time`, so every guard works, and supports server-side
    /// filtering to the pump program. A create arrives *after* its block is
    /// built, so block 1 is the floor.
    Yellowstone,
    /// Jito shredstream. Pre-confirmation shreds, so this is the only route to
    /// block 0 — paid for with fabricated metadata, no server-side filter, and
    /// no execution status.
    Shredstream,
}

impl DatasourceKind {
    /// True when the datasource fabricates `TransactionStatusMeta` instead of
    /// reporting the chain's. Shredstream carries shreds, which exist before
    /// execution, so there are no balances and no status to report — the
    /// datasource fills in `status: Ok(())` and an empty meta, and uses local
    /// receive time for `block_time`.
    ///
    /// Guards that read `meta.pre_balances` / `meta.post_balances` /
    /// `block_time` therefore cannot work. They are skipped explicitly rather
    /// than left to silently compare zeroes.
    pub fn has_synthetic_meta(&self) -> bool {
        matches!(self, DatasourceKind::Shredstream)
    }
}

/// Where the market tracker's `TradeEvent`s come from.
///
/// Pump publishes fills as an Anchor self-CPI event, which lands in the
/// transaction's **inner** instructions at execution time. Carbon reads CPI
/// events out of `meta.inner_instructions`, so a feed that carries no real
/// `TransactionStatusMeta` carries no `TradeEvent`s either — and that is exactly
/// what shredstream is, since a shred holds the transaction as *submitted*, not
/// as *executed*. Detection still works there (top-level `create` is in the
/// submitted message); only price/volume needs a meta-bearing feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketFeed {
    /// The primary datasource already carries real meta, so `TradeEvent`s
    /// decode from it directly and nothing extra is subscribed.
    Native,
    /// The primary carries no meta; a second Yellowstone subscription at this
    /// URL runs alongside it purely to supply inner instructions.
    Secondary(String),
    /// The primary carries no meta and no secondary is configured. Detection
    /// works; live price/volume does not.
    Unavailable,
}

/// Decide how `TradeEvent`s will reach the market tracker.
///
/// Split out as a pure function so the precedence — the primary feed's own
/// metadata always wins over `MARKET_GEYSER_URL` — is pinned by tests rather
/// than buried in the wiring. Attaching a second Yellowstone stream when the
/// primary is already Yellowstone would double every update through the same
/// processor for no gain.
fn resolve_market_feed(datasource: DatasourceKind, market_geyser_url: Option<&str>) -> MarketFeed {
    match datasource {
        DatasourceKind::Yellowstone => MarketFeed::Native,
        DatasourceKind::Shredstream => match market_geyser_url.map(str::trim) {
            Some(url) if !url.is_empty() => MarketFeed::Secondary(url.to_string()),
            _ => MarketFeed::Unavailable,
        },
    }
}

/// One buyer wallet plus its per-wallet dispatch parameters.
pub struct Buyer {
    pub keypair: Keypair,
    /// Lamports this wallet spends on a snipe.
    ///
    /// Atomic because in `BuySizing::Balance` mode it is derived from the
    /// wallet's live balance, which is only known after the Config is built
    /// and wrapped in an `Arc`. A relaxed load on the hot path is free, and it
    /// leaves room to re-derive between snipes.
    buy_amount_lamports: std::sync::atomic::AtomicU64,
    pub priority_fee_micro_lamports: u64,
    /// Index into `Config::fast_providers`: which fast/anti-MEV endpoint this
    /// wallet's transaction is built and submitted for (its tip is baked in at
    /// build time). `None` means plain RPC spray only, no tip.
    pub provider: Option<usize>,
}

impl Buyer {
    pub fn buy_amount_lamports(&self) -> u64 {
        self.buy_amount_lamports
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_buy_amount_lamports(&self, lamports: u64) {
        self.buy_amount_lamports
            .store(lamports, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How each wallet's buy size is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuySizing {
    /// Every wallet spends `BUY_AMOUNT_SOL`.
    Fixed,
    /// Each wallet spends a random `BUY_BALANCE_PCT_MIN`..`BUY_BALANCE_PCT_MAX`
    /// percent of its own spendable balance (balance minus the fee reserve).
    ///
    /// This is the setting that makes thirty buys look like thirty people.
    /// The amounts inherit the variance already present in the funding — which
    /// was jittered, and spread over hours or days — so no two wallets buy the
    /// same number, and each one apes very nearly everything it holds, which is
    /// what an organic retail buyer does. Fixed mode emits thirty byte-
    /// identical buys in one block, which is the literal definition of the
    /// cluster every terminal scans for.
    Balance,
}

pub struct Config {
    /// How each wallet's buy size is decided. `BUY_SIZING`, default `fixed`.
    pub buy_sizing: BuySizing,
    /// Percentage band of spendable balance to buy with in `BuySizing::Balance`.
    pub buy_balance_pct_min: u64,
    pub buy_balance_pct_max: u64,
    /// Which feed creates are detected on. `DATASOURCE`, default `yellowstone`.
    pub datasource: DatasourceKind,
    /// Required when `datasource` is `Yellowstone`.
    pub geyser_url: Option<String>,
    /// Required when `datasource` is `Shredstream`.
    pub shredstream_url: Option<String>,
    /// `x-token` metadata for the shredstream proxy. Falls back to `X_TOKEN`.
    pub shredstream_x_token: Option<String>,
    pub x_token: Option<String>,
    /// How live price/volume reaches the market tracker. Derived from
    /// `datasource` + `MARKET_GEYSER_URL`, never set directly.
    pub market_feed: MarketFeed,
    /// `x-token` for the market-only Yellowstone stream. `MARKET_X_TOKEN`,
    /// falling back to `X_TOKEN` — the market endpoint is usually the same
    /// provider as `GEYSER_URL` would be.
    pub market_x_token: Option<String>,
    pub rpc_urls: Vec<String>,

    pub watched_creators: HashSet<Pubkey>,
    pub blacklisted_creators: HashSet<Pubkey>,

    pub buyers: Vec<Buyer>,
    pub slippage_bps: u64,
    pub track_volume: bool,

    /// Snipe `create_v2` launches as well as `create` ones. v2 coins are
    /// Token-2022 and trade against a quote mint, so each buy also has to wrap
    /// SOL — a heavier transaction than a v1 buy. Off leaves v2 launches logged
    /// and skipped, which is the pre-v2 behaviour.
    pub snipe_v2: bool,
    /// Close the buyer's wrapped-SOL account in the same transaction as a v2
    /// buy, returning unspent WSOL and the account rent. Costs one instruction;
    /// leaving it off keeps the ATA around for the next buy.
    pub unwrap_after_buy: bool,

    pub min_creator_balance_lamports: u64,
    /// Doubles as the guard ceiling and, when the create transaction has no
    /// recognisable dev buy, the fallback estimate fed to the quote. Only the
    /// guard half depends on real metadata.
    pub max_creator_buy_lamports: u64,
    pub max_positions: usize,
    pub max_tx_age_ms: i64,

    /// Extra attempts if a snipe lands nothing. Only fires when **zero** buys
    /// landed — a partial fill is a success, and resending would double a
    /// position. Each attempt pays priority fees again. `SNIPE_RETRIES`.
    pub snipe_retries: u32,

    /// Use the modelled slippage floor for v2 buys. Off by default because the
    /// v2 quote is known wrong: `Global` carries no v2 token reserve, so the
    /// model overstates output ~5.8x and the program rejects the buy with
    /// BuySlippageBelowMinTokensOut (6042). Turn on only after the reserve is
    /// read from the bonding curve. `V2_TRUST_QUOTE`.
    pub v2_trust_quote: bool,

    /// Log every decoded launch before the watched-creator filter, with the
    /// creator/user/fee_payer fields the filter tests. Diagnostic only: it
    /// distinguishes "launch never arrived on the feed" from "arrived but did
    /// not match", which the silent filter otherwise hides. `LOG_ALL_CREATES`.
    pub log_all_creates: bool,

    /// Set when the datasource fabricates transaction metadata, so the
    /// balance- and `block_time`-derived guards must be skipped rather than
    /// evaluated against zeroes. Derived from `datasource`, never set directly.
    pub synthetic_meta: bool,

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
        let datasource = match env::var("DATASOURCE")
            .unwrap_or_else(|_| "yellowstone".into())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "yellowstone" | "geyser" => DatasourceKind::Yellowstone,
            "shredstream" | "shreds" | "prism" => DatasourceKind::Shredstream,
            other => {
                return Err(format!(
                    "DATASOURCE must be 'yellowstone' or 'shredstream', got '{other}'"
                ))
            }
        };
        let synthetic_meta = datasource.has_synthetic_meta();

        // Each feed needs its own endpoint; require only the one in use so a
        // shredstream-only deployment doesn't have to invent a GEYSER_URL.
        let (geyser_url, shredstream_url) = match datasource {
            DatasourceKind::Yellowstone => (Some(require("GEYSER_URL")?), None),
            DatasourceKind::Shredstream => (None, Some(require("SHREDSTREAM_URL")?)),
        };
        if let Some(url) = shredstream_url.as_deref() {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(format!(
                    "SHREDSTREAM_URL must start with http:// or https:// (tonic does not accept a \
                     grpc:// scheme). Got '{url}' — a TLS proxy is almost always https://"
                ));
            }
        }
        let shredstream_x_token = env::var("SHREDSTREAM_X_TOKEN")
            .ok()
            .or_else(|| env::var("X_TOKEN").ok());
        if datasource == DatasourceKind::Shredstream && shredstream_x_token.is_none() {
            log::warn!(
                "DATASOURCE=shredstream with no SHREDSTREAM_X_TOKEN/X_TOKEN — hosted proxies \
                 reject unauthenticated streams with a bare HTTP 204, which surfaces as \
                 'malformed header: missing HTTP content-type' rather than an auth error"
            );
        }

        let market_feed =
            resolve_market_feed(datasource, env::var("MARKET_GEYSER_URL").ok().as_deref());
        let market_x_token = env::var("MARKET_X_TOKEN")
            .ok()
            .or_else(|| env::var("X_TOKEN").ok());

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
        // Per-wallet buy size spread. Thirty buys of a byte-identical amount,
        // in one block, is the strongest cluster signature the sniper can
        // emit — stronger than anything in the funding graph, and exactly what
        // a slot-window bundle scanner keys on. Organic buyers land on
        // scattered sizes; thirty equal ones do not occur naturally.
        //
        // How buy sizes are decided. `balance` is the mode that makes thirty
        // buys look like thirty people: each wallet spends nearly all of what
        // it holds, and what it holds was funded at a jittered amount at a
        // random time, so the variance is inherited rather than invented.
        //
        // The amounts cannot be computed here — they need live balances, which
        // need RPC, which does not exist at config load. `default_buy` is a
        // placeholder that `wallets::preflight_balances` overwrites.
        // An empty value is how a `.env` key is normally disabled, and
        // `SEND_MODE` already treats it as "default" — so this does too rather
        // than aborting startup on `BUY_SIZING=`.
        let buy_sizing = match env::var("BUY_SIZING")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "fixed".into())
            .to_lowercase()
            .as_str()
        {
            "balance" => BuySizing::Balance,
            "fixed" => BuySizing::Fixed,
            other => {
                return Err(format!(
                    "BUY_SIZING must be 'fixed' or 'balance', got '{other}'"
                ))
            }
        };
        let buy_balance_pct_min = num_env("BUY_BALANCE_PCT_MIN", 98)?;
        let buy_balance_pct_max = num_env("BUY_BALANCE_PCT_MAX", 100)?;
        if buy_balance_pct_min > buy_balance_pct_max {
            return Err(format!(
                "BUY_BALANCE_PCT_MIN ({buy_balance_pct_min}) exceeds BUY_BALANCE_PCT_MAX \
                 ({buy_balance_pct_max})"
            ));
        }
        if buy_sizing == BuySizing::Balance && buy_balance_pct_min == 0 {
            return Err(
                "BUY_BALANCE_PCT_MIN is 0: a wallet would buy nothing, and a 0-lamport buy still \
                 pays priority fees and tips. Set it to the smallest share you actually want."
                    .into(),
            );
        }
        if buy_balance_pct_max > 100 {
            return Err(format!(
                "BUY_BALANCE_PCT_MAX is {buy_balance_pct_max}: a wallet cannot spend more than \
                 100% of what it holds beyond its fee reserve"
            ));
        }
        let buyers = keypairs
            .into_iter()
            .enumerate()
            .map(|(i, keypair)| Buyer {
                keypair,
                buy_amount_lamports: std::sync::atomic::AtomicU64::new(default_buy),
                priority_fee_micro_lamports: if n > 1 {
                    base_fee + jitter * i as u64 / (n - 1)
                } else {
                    base_fee
                },
                provider: use_fast.then(|| i % fast_providers.len()),
            })
            .collect();

        let min_creator_balance_lamports = sol_env("MIN_CREATOR_BALANCE_SOL", 0.0)?;
        if synthetic_meta {
            // These read meta.pre_balances / meta.post_balances / block_time,
            // none of which shredstream carries. Refuse the one whose failure
            // is a silent kill switch (the balance floor rejects every launch
            // once every balance reads 0), and say plainly that the other two
            // are inactive rather than letting them look enforced.
            validate_meta_dependent_guards(synthetic_meta, min_creator_balance_lamports)?;
            if env::var("MAX_CREATOR_BUY_SOL").is_ok() {
                log::warn!(
                    "MAX_CREATOR_BUY_SOL is set but its GUARD is inactive on \
                     DATASOURCE=shredstream (no balances in shreds). The value is still used as \
                     the dev-buy fallback when a create has no recognisable buy instruction."
                );
            }
            if env::var("MAX_TX_AGE_MS").is_ok() {
                log::warn!(
                    "MAX_TX_AGE_MS is set but the freshness gate is inactive on \
                     DATASOURCE=shredstream: block_time is the local receive time, so every \
                     create measures as ~0ms old."
                );
            }
            log::warn!(
                "DATASOURCE=shredstream: shreds are PRE-CONFIRMATION, so a create seen here may \
                 never land, and there is no server-side program filter — every transaction on \
                 the network is decoded locally."
            );
        }

        Ok(Self {
            buy_sizing,
            buy_balance_pct_min,
            buy_balance_pct_max,
            datasource,
            geyser_url,
            shredstream_url,
            shredstream_x_token,
            market_feed,
            market_x_token,
            synthetic_meta,
            snipe_retries: num_env("SNIPE_RETRIES", 1)? as u32,
            v2_trust_quote: bool_env("V2_TRUST_QUOTE", false),
            log_all_creates: bool_env("LOG_ALL_CREATES", false),
            x_token: env::var("X_TOKEN").ok(),
            rpc_urls,
            watched_creators,
            blacklisted_creators,
            buyers,
            slippage_bps: num_env("SLIPPAGE_BPS", 500)?,
            track_volume: env::var("TRACK_VOLUME").is_ok_and(|v| v == "true"),
            snipe_v2: bool_env("SNIPE_V2", true),
            unwrap_after_buy: bool_env("UNWRAP_AFTER_BUY", true),
            min_creator_balance_lamports,
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

/// Reject configurations whose guards depend on metadata the datasource does not
/// carry and whose failure mode is silent.
///
/// The two balance guards fail in **opposite** directions when every balance
/// reads as 0, which is why only one of them is a hard error:
///
/// - `MIN_CREATOR_BALANCE_SOL` fails **closed**. `pre` is 0, and `0 < minimum`
///   is TRUE for any minimum above zero, so the guard rejects *every* launch.
///   The sniper would sit there watching the right creator and never fire, with
///   no error to explain it — a silent kill switch. Hence the hard error here.
/// - `MAX_CREATOR_BUY_SOL` fails **open**. `pre` and `post` are both 0, so
///   `spent` is 0 and never exceeds the ceiling: every launch passes a guard the
///   operator believes is filtering. `MAX_TX_AGE_MS` is open in the same way
///   (`block_time` is the local receive time, so creates measure ~0ms old).
///   Both are warnings at startup rather than errors, because their defaults
///   are permissive and neither stops the bot from working.
///
/// The guards themselves are skipped wholesale on a synthetic-meta feed (see
/// `processor.rs`); this function exists to make the one dangerous *setting*
/// visible at startup rather than at 3am.
fn validate_meta_dependent_guards(
    synthetic_meta: bool,
    min_creator_balance_lamports: u64,
) -> Result<(), String> {
    if synthetic_meta && min_creator_balance_lamports > 0 {
        return Err(
            "MIN_CREATOR_BALANCE_SOL cannot be enforced on DATASOURCE=shredstream: shreds carry \
             no balances, so every creator's balance reads as 0 and the floor would reject every \
             launch. Unset it, or use DATASOURCE=yellowstone."
                .into(),
        );
    }
    Ok(())
}

/// Boolean env var with an explicit default, so a flag can default to on and
/// still be switched off with `=false`.
fn bool_env(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => default,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_shredstream_has_synthetic_meta() {
        assert!(!DatasourceKind::Yellowstone.has_synthetic_meta());
        assert!(DatasourceKind::Shredstream.has_synthetic_meta());
    }

    #[test]
    fn a_creator_balance_floor_is_rejected_on_a_synthetic_meta_feed() {
        // The failure this prevents is silent: with no balances in the update,
        // pre reads as 0, `0 < floor` is TRUE, and every launch is *rejected*
        // while the operator believes a floor is merely filtering. See
        // `processor::tests` for the guard direction itself.
        let err = validate_meta_dependent_guards(true, 1)
            .expect_err("a non-zero floor on shredstream must be refused, not silently ignored");
        assert!(
            err.contains("MIN_CREATOR_BALANCE_SOL") && err.contains("shredstream"),
            "error should name the variable and the feed, got: {err}"
        );
        assert!(
            err.contains("reject"),
            "the error must teach the real direction — the floor rejects every launch, it does \
             not admit them. Got: {err}"
        );
    }

    #[test]
    fn a_creator_balance_floor_is_allowed_on_a_real_meta_feed() {
        assert!(validate_meta_dependent_guards(false, 5_000_000_000).is_ok());
    }

    #[test]
    fn yellowstone_tracks_the_market_natively_and_ignores_a_market_url() {
        // A second Yellowstone stream would deliver the same updates twice into
        // the same processor for no gain, so the primary's own meta wins even
        // when MARKET_GEYSER_URL is set.
        assert_eq!(
            resolve_market_feed(DatasourceKind::Yellowstone, None),
            MarketFeed::Native
        );
        assert_eq!(
            resolve_market_feed(DatasourceKind::Yellowstone, Some("https://geyser.example")),
            MarketFeed::Native
        );
    }

    #[test]
    fn shredstream_takes_a_secondary_market_feed_when_one_is_given() {
        assert_eq!(
            resolve_market_feed(DatasourceKind::Shredstream, Some(" https://geyser.example ")),
            MarketFeed::Secondary("https://geyser.example".into()),
            "surrounding whitespace is an env-var artefact, not part of the endpoint"
        );
    }

    #[test]
    fn shredstream_alone_cannot_track_the_market() {
        // Not an error: detection is the point of shredstream and still works.
        // Only price/volume needs inner instructions, which shreds lack.
        assert_eq!(
            resolve_market_feed(DatasourceKind::Shredstream, None),
            MarketFeed::Unavailable
        );
        assert_eq!(
            resolve_market_feed(DatasourceKind::Shredstream, Some("   ")),
            MarketFeed::Unavailable,
            "an empty/blank MARKET_GEYSER_URL is unset, not an endpoint to dial"
        );
    }

    #[test]
    fn a_zero_floor_is_allowed_on_either_feed() {
        // Zero is the default and means "no floor", so it is not a guard the
        // operator is relying on and does not need refusing.
        assert!(validate_meta_dependent_guards(true, 0).is_ok());
        assert!(validate_meta_dependent_guards(false, 0).is_ok());
    }
}
