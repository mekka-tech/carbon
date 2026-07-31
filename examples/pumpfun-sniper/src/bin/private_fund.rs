//! Create private (Monero-routed) funding orders for the sniper wallets.
//!
//! ```text
//!   private_fund                      # quote only, create nothing
//!   private_fund --execute            # create the orders, write deposits.txt
//!   private_fund --status <multiId>   # (or --status-order <houdiniId>)
//! ```
//!
//! This does not move any funds. It asks Houdini for one order per wallet,
//! each routed through Monero, and writes the deposit addresses it returns to
//! `deposits.txt`. Sending is then the already-tested path:
//!
//! ```text
//!   private_fund --execute
//!   distribute --deposits deposits.txt --execute
//! ```
//!
//! Splitting it this way is deliberate. Order creation is idempotent-ish and
//! reversible (an unfunded order simply expires); sending is neither. Keeping
//! them in separate commands means the addresses can be read, checked, and
//! sanity-checked against the quote before a single lamport moves.
//!
//! # Why a provider rather than intermediate hops
//!
//! Hop wallets do not provide privacy, they provide *depth*: source → hop →
//! wallet is a directed path with an anonymity set of exactly one, and walking
//! it is trivial. A Monero route has a real anonymity set, so the link is
//! broken rather than lengthened. `useXmr` and `anonymous` below are what buy
//! that, and they are set on every order.
//!
//! # Untested against the live API
//!
//! Schemas here are transcribed from Houdini's OpenAPI spec, not exercised —
//! that needs a partner key. Every response is therefore validated rather than
//! trusted, and the failure mode is a loud error with the raw body attached.
//! In particular `deposit_address_is_solana` refuses any address that is not a
//! valid Solana pubkey: a wrong-chain deposit address would otherwise send SOL
//! somewhere unrecoverable.
//!
//! Reads `HOUDINI_API_KEY`, `HOUDINI_API_SECRET`, `BUYER_KEYPAIR_DIR`,
//! `RPC_URLS`, and `DIST_TARGET_SOL` / `DIST_JITTER_PCT`.

use {
    solana_client::nonblocking::rpc_client::RpcClient, solana_commitment_config::CommitmentConfig,
    solana_keypair::Keypair, solana_pubkey::Pubkey, solana_signer::Signer, std::str::FromStr,
    std::sync::Arc,
};

const API_BASE: &str = "https://api-partner.houdiniswap.com/v2";
const DEPOSITS_FILE: &str = "deposits.txt";

/// Houdini rejects requests without these compliance headers with a 400.
fn compliance_headers(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.header("x-user-ip", std::env::var("HOUDINI_USER_IP").unwrap_or_default())
        .header(
            "x-user-agent",
            std::env::var("HOUDINI_USER_AGENT")
                .unwrap_or_else(|_| "pumpfun-sniper/1.0".to_string()),
        )
        .header(
            "x-user-timezone",
            std::env::var("HOUDINI_USER_TZ").unwrap_or_else(|_| "UTC".to_string()),
        )
}

fn auth_header() -> Result<String, String> {
    let key = std::env::var("HOUDINI_API_KEY")
        .map_err(|_| "set HOUDINI_API_KEY (Houdini partner portal, free tier)".to_string())?;
    let secret = std::env::var("HOUDINI_API_SECRET")
        .map_err(|_| "set HOUDINI_API_SECRET".to_string())?;
    // Documented format: `Authorization: <ApiKey>:<ApiSecret>`.
    Ok(format!("{key}:{secret}"))
}

/// A deposit address must be a Solana address, whatever the provider says.
///
/// The route is SOL → XMR → SOL, so the *deposit* leg is Solana. If a schema
/// change or a mis-specified chain ever returns a Monero or EVM address here,
/// sending SOL to it destroys the funds with no recourse — so this is checked
/// rather than assumed, and a failure aborts the whole run.
fn deposit_address_is_solana(addr: &str) -> Result<Pubkey, String> {
    Pubkey::from_str(addr).map_err(|_| {
        format!(
            "provider returned '{addr}', which is not a Solana address — refusing to write it \
             as a deposit target. Sending SOL there would be unrecoverable."
        )
    })
}

/// `GET /quotes/byChainAddress` — avoids needing Houdini's internal 24-char
/// token ObjectIds, which `/quotes` and `POST /exchanges/multi` both require.
async fn quote(
    http: &reqwest::Client,
    auth: &str,
    amount_sol: f64,
) -> Result<(String, f64), String> {
    let url = format!("{API_BASE}/quotes/byChainAddress");
    let resp = compliance_headers(
        http.get(&url)
            .header("Authorization", auth)
            .query(&[
                ("amount", amount_sol.to_string()),
                ("fromChain", "SOL".to_string()),
                ("toChain", "SOL".to_string()),
            ]),
    )
    .send()
    .await
    .map_err(|e| format!("quote request failed: {e}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("quote HTTP {status}: {body}"));
    }
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("quote is not JSON ({e}): {body}"))?;
    // Take the first quote offered. `quotes[]` is ordered by the provider.
    let first = v
        .get("quotes")
        .and_then(|q| q.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| format!("no quotes in response: {body}"))?;
    let quote_id = first
        .get("quoteId")
        .and_then(|q| q.as_str())
        .ok_or_else(|| format!("quote has no quoteId: {body}"))?
        .to_string();
    let amount_out = first
        .get("amountOut")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| format!("quote has no amountOut: {body}"))?;
    Ok((quote_id, amount_out))
}

/// `POST /exchanges` — `{ addressTo, quoteId }` → `{ houdiniId, depositAddress }`.
async fn create_exchange(
    http: &reqwest::Client,
    auth: &str,
    quote_id: &str,
    address_to: &Pubkey,
    refund_address: Option<&str>,
) -> Result<(String, Pubkey), String> {
    let mut body = serde_json::json!({
        "quoteId": quote_id,
        "addressTo": address_to.to_string(),
        // The whole point. `useXmr` routes through Monero; `anonymous` opts out
        // of the non-private direct path.
        "useXmr": true,
        "anonymous": true,
    });
    // Without a refund address a failed or under/over-funded swap has nowhere
    // to return the principal.
    if let Some(refund) = refund_address {
        body["refundAddress"] = serde_json::Value::String(refund.to_string());
    }

    let resp = compliance_headers(
        http.post(format!("{API_BASE}/exchanges"))
            .header("Authorization", auth)
            .json(&body),
    )
    .send()
    .await
    .map_err(|e| format!("exchange request failed: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("exchange HTTP {status}: {text}"));
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("exchange is not JSON ({e}): {text}"))?;
    let houdini_id = v
        .get("houdiniId")
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("no houdiniId in response: {text}"))?
        .to_string();
    let deposit = v
        .get("depositAddress")
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("no depositAddress in response: {text}"))?;
    Ok((houdini_id, deposit_address_is_solana(deposit)?))
}

fn lamports_to_sol(lamports: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let sol = lamports as f64 / 1e9;
    sol
}

/// Destination wallets in the sniper's own load order.
fn load_destinations() -> Result<Vec<(String, Pubkey)>, String> {
    let dir = std::env::var("BUYER_KEYPAIR_DIR")
        .map_err(|_| "set BUYER_KEYPAIR_DIR".to_string())?;
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| format!("cannot read {dir}: {e}"))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let raw = std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            let bytes: Vec<u8> = serde_json::from_str(raw.trim())
                .map_err(|e| format!("bad keypair JSON in {}: {e}", p.display()))?;
            let kp = Keypair::try_from(bytes.as_slice())
                .map_err(|e| format!("bad keypair in {}: {e}", p.display()))?;
            Ok((
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("wallet")
                    .to_string(),
                kp.pubkey(),
            ))
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), String> {
    dotenv::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let execute = args.iter().any(|a| a == "--execute");
    let value_of = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i.saturating_add(1)))
            .cloned()
    };

    let auth = auth_header()?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    // Status lookups exit early — they touch nothing.
    if let Some(multi_id) = value_of("--status") {
        let resp = compliance_headers(
            http.get(format!("{API_BASE}/exchanges/multi/{multi_id}"))
                .header("Authorization", &auth),
        )
        .send()
        .await
        .map_err(|e| format!("status request failed: {e}"))?;
        println!("{}", resp.text().await.unwrap_or_default());
        return Ok(());
    }
    if let Some(order) = value_of("--status-order") {
        let resp = compliance_headers(
            http.get(format!("{API_BASE}/orders/{order}"))
                .header("Authorization", &auth),
        )
        .send()
        .await
        .map_err(|e| format!("status request failed: {e}"))?;
        println!("{}", resp.text().await.unwrap_or_default());
        return Ok(());
    }

    let target_sol: f64 = value_of("--target")
        .and_then(|v| v.parse().ok())
        .or_else(|| {
            std::env::var("DIST_TARGET_SOL")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0.15);

    // Only quote for wallets that actually need funding, so a re-run after a
    // partial round does not pay for thirty swaps to top up three wallets.
    let rpc_url = std::env::var("RPC_URLS")
        .map_err(|_| "set RPC_URLS".to_string())?
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let rpc = Arc::new(RpcClient::new_with_commitment(
        rpc_url,
        CommitmentConfig::confirmed(),
    ));

    let refund = std::env::var("HOUDINI_REFUND_ADDRESS").ok();
    if refund.is_none() {
        println!(
            "WARNING: HOUDINI_REFUND_ADDRESS is unset. A swap that fails or falls outside its \
             quoted band has nowhere to return the principal."
        );
    }

    let destinations = load_destinations()?;
    let mut needed: Vec<(String, Pubkey)> = Vec::new();
    for (label, pk) in destinations {
        let bal = rpc.get_balance(&pk).await.unwrap_or(0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let target_lamports = (target_sol * 1e9) as u64;
        if bal.saturating_add(1_000_000) < target_lamports {
            needed.push((label, pk));
        } else {
            println!("  {label} already at {:.6} SOL — skipping", lamports_to_sol(bal));
        }
    }
    if needed.is_empty() {
        println!("every wallet is already funded; nothing to do");
        return Ok(());
    }

    println!();
    println!(
        "{} wallet(s) to fund at ~{target_sol} SOL each, routed SOL → XMR → SOL",
        needed.len()
    );
    if !execute {
        // Quote anyway. Quotes create nothing and cost nothing, and they are
        // the only honest answer to "what does this charge": Houdini bills no
        // explicit user fee, so the entire cost — their ~0.5% partner
        // commission, the spread on BOTH legs of SOL -> XMR -> SOL, and the
        // Monero network fee — is embedded in the rate. `amountOut` is what
        // actually arrives, so the difference is the real, all-in price.
        println!("quoting (creates nothing)…");
        println!();
        let (mut total_in, mut total_out, mut quoted) = (0.0f64, 0.0f64, 0usize);
        for (label, _) in &needed {
            match quote(&http, &auth, target_sol).await {
                Ok((_, amount_out)) => {
                    let lost = target_sol - amount_out;
                    let pct = if target_sol > 0.0 {
                        lost / target_sol * 100.0
                    } else {
                        0.0
                    };
                    println!(
                        "  {label:<14} send {target_sol:.6} → receive {amount_out:.6} SOL                           (cost {lost:.6} SOL, {pct:.2}%)"
                    );
                    total_in += target_sol;
                    total_out += amount_out;
                    quoted = quoted.saturating_add(1);
                }
                Err(e) => println!("  {label:<14} QUOTE FAILED: {e}"),
            }
        }
        if quoted > 0 {
            let lost = total_in - total_out;
            let pct = if total_in > 0.0 {
                lost / total_in * 100.0
            } else {
                0.0
            };
            println!();
            println!(
                "{quoted} quote(s): send {total_in:.6} SOL → receive {total_out:.6} SOL"
            );
            println!("ALL-IN COST {lost:.6} SOL ({pct:.2}%) — rate spread, not a line-item fee");
        }
        println!();
        println!("DRY RUN — no orders created. Re-run with --execute.");
        println!("Each order will be created with useXmr=true, anonymous=true.");
        return Ok(());
    }

    println!();
    let mut lines: Vec<String> = vec![
        "# Houdini deposit addresses. Amounts are the provider's quote and are".to_string(),
        "# used verbatim by `distribute --deposits` — do NOT edit them.".to_string(),
    ];
    let mut orders: Vec<(String, String)> = Vec::new();
    let mut failed = 0usize;
    for (label, pk) in &needed {
        match quote(&http, &auth, target_sol).await {
            Ok((quote_id, amount_out)) => {
                match create_exchange(&http, &auth, &quote_id, pk, refund.as_deref()).await {
                    Ok((houdini_id, deposit)) => {
                        println!(
                            "  {label:<14} → {pk}  deposit {deposit}  (out ~{amount_out:.6} SOL)  {houdini_id}"
                        );
                        lines.push(format!("{deposit},{target_sol}"));
                        orders.push((label.clone(), houdini_id));
                    }
                    Err(e) => {
                        failed = failed.saturating_add(1);
                        println!("  {label:<14} EXCHANGE FAILED: {e}");
                    }
                }
            }
            Err(e) => {
                failed = failed.saturating_add(1);
                println!("  {label:<14} QUOTE FAILED: {e}");
            }
        }
    }

    if orders.is_empty() {
        return Err("no orders were created — nothing written".into());
    }
    std::fs::write(DEPOSITS_FILE, lines.join("\n"))
        .map_err(|e| format!("cannot write {DEPOSITS_FILE}: {e}"))?;
    println!();
    println!("{} order(s) created, {failed} failed", orders.len());
    println!("wrote {DEPOSITS_FILE}");
    println!();
    println!("Next, and only after reading that file:");
    println!("  distribute --deposits {DEPOSITS_FILE} --execute");
    println!();
    println!("Track with: private_fund --status-order <houdiniId>");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solana_deposit_address_is_accepted() {
        let pk = Pubkey::new_unique().to_string();
        assert!(deposit_address_is_solana(&pk).is_ok());
    }

    #[test]
    fn a_non_solana_deposit_address_is_refused() {
        // The failure this guards: the deposit leg is SOL, so an XMR or EVM
        // address here means SOL sent somewhere unrecoverable. Better to abort
        // the run than to write it into deposits.txt.
        for bad in [
            "0x71C7656EC7ab88b098defB751B7401B5f6d8976F", // EVM
            "4AdUndXHHZ6cfufTMvppY6JwXNouMBzSkbLYfpAV5Usx3skxNgYeYTRj5UzqtReoS44qo9mtmXCqY45DJ852K5Jv2684Rge", // XMR
            "",
            "not-an-address",
        ] {
            assert!(
                deposit_address_is_solana(bad).is_err(),
                "accepted a non-Solana address: {bad}"
            );
        }
    }

    #[test]
    fn the_refusal_explains_the_stakes() {
        let err = deposit_address_is_solana("0xdeadbeef").unwrap_err();
        assert!(err.contains("unrecoverable"), "{err}");
    }
}
