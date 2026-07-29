use {
    base64::Engine, solana_instruction::Instruction, solana_pubkey::Pubkey,
    solana_system_interface::instruction as system_instruction,
    solana_transaction::versioned::VersionedTransaction,
};

/// Jito enforces a maximum of 5 transactions per bundle.
pub const MAX_BUNDLE_SIZE: usize = 5;

/// Jito mainnet tip accounts. Spread across them so 30 buys don't all
/// write-lock a single account.
const TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

pub fn tip_instruction(payer: &Pubkey, lamports: u64) -> Instruction {
    // Deterministic per-payer spread across the tip accounts.
    let index = payer.as_ref()[0] as usize % TIP_ACCOUNTS.len();
    let tip_account = Pubkey::from_str_const(TIP_ACCOUNTS[index]);
    system_instruction::transfer(payer, &tip_account, lamports)
}

/// Split the transactions into bundles of 5 and submit them concurrently,
/// **one bundle per regional endpoint**.
///
/// Jito rate-limits to 1 request per second per IP per region, so firing six
/// bundles at a single endpoint would 429 five of them. Bundles are therefore
/// dealt round-robin across the configured regions; with 30 buys that means 6
/// bundles across 6+ regions, one request each. Regions further from the box
/// cost latency, so order `JITO_BLOCK_ENGINE_URLS` nearest-first.
///
/// Runs alongside the RPC spray, never instead of it: bundles only help when
/// the next leader is a Jito leader.
pub async fn send_bundles(block_engine_urls: &[String], txs: &[VersionedTransaction]) {
    if block_engine_urls.is_empty() {
        return;
    }
    let client = reqwest::Client::new();
    let bundle_count = txs.len().div_ceil(MAX_BUNDLE_SIZE);
    if bundle_count > block_engine_urls.len() {
        log::warn!(
            "{bundle_count} jito bundles across only {} region(s) — Jito allows 1 request/s per \
             region per IP, so some will be rate limited; add more regions to JITO_BLOCK_ENGINE_URLS",
            block_engine_urls.len()
        );
    }

    let sends = txs.chunks(MAX_BUNDLE_SIZE).enumerate().map(|(i, chunk)| {
        let client = client.clone();
        let base = &block_engine_urls[i % block_engine_urls.len()];
        let url = format!("{}/api/v1/bundles", base.trim_end_matches('/'));
        async move { (i, send_one_bundle(&client, &url, chunk).await) }
    });

    let mut ok = 0usize;
    for (i, result) in futures::future::join_all(sends).await {
        match result {
            Ok(response) => {
                ok += 1;
                log::debug!("jito bundle #{i} submitted: {response}");
            }
            Err(err) => log::warn!("jito bundle #{i} failed: {err}"),
        }
    }
    log::info!(
        "jito: {ok}/{} bundles submitted",
        txs.len().div_ceil(MAX_BUNDLE_SIZE)
    );
}

async fn send_one_bundle(
    client: &reqwest::Client,
    url: &str,
    txs: &[VersionedTransaction],
) -> Result<String, String> {
    let encoded: Vec<String> = txs
        .iter()
        .map(|tx| {
            bincode::serialize(tx)
                .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
                .map_err(|e| format!("serialize tx: {e}"))
        })
        .collect::<Result<_, _>>()?;

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendBundle",
        "params": [encoded, {"encoding": "base64"}],
    });

    let response = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("jito request: {e}"))?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_success() {
        Ok(text)
    } else {
        Err(format!("jito sendBundle failed ({status}): {text}"))
    }
}
