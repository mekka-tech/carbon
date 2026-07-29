use {
    base64::Engine, solana_instruction::Instruction, solana_pubkey::Pubkey,
    solana_system_interface::instruction as system_instruction,
    solana_transaction::versioned::VersionedTransaction,
};

/// Jito mainnet tip accounts (any one works; rotate to spread writes).
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
    let tip_account = Pubkey::from_str_const(TIP_ACCOUNTS[0]);
    system_instruction::transfer(payer, &tip_account, lamports)
}

/// Submit all transactions as one atomic bundle via the block engine's
/// JSON-RPC `sendBundle` (max 5 txs per bundle).
pub async fn send_bundle(
    block_engine_url: &str,
    txs: &[VersionedTransaction],
) -> Result<(), String> {
    if txs.len() > 5 {
        return Err(format!(
            "jito bundles allow at most 5 txs, got {}",
            txs.len()
        ));
    }
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

    let url = format!("{}/api/v1/bundles", block_engine_url.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("jito request: {e}"))?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_success() {
        log::info!("jito bundle submitted: {text}");
        Ok(())
    } else {
        Err(format!("jito sendBundle failed ({status}): {text}"))
    }
}
