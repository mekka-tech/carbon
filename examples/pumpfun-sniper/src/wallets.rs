use {
    crate::config::Config, solana_client::nonblocking::rpc_client::RpcClient, solana_signer::Signer,
};

/// Batch-check every buyer wallet's balance before going live. A wallet that
/// cannot cover its buy plus fees and rent is a dead transaction at snipe
/// time, so surface it now rather than in the middle of a launch.
///
/// Returns the indices of wallets that are underfunded.
pub async fn preflight_balances(cfg: &Config, rpc: &RpcClient) -> Result<Vec<usize>, String> {
    let pubkeys: Vec<_> = cfg.buyers.iter().map(|b| b.keypair.pubkey()).collect();

    let mut balances = Vec::with_capacity(pubkeys.len());
    // get_multiple_accounts caps at 100 keys per call.
    for chunk in pubkeys.chunks(100) {
        let accounts = rpc
            .get_multiple_accounts(chunk)
            .await
            .map_err(|e| format!("balance preflight failed: {e}"))?;
        balances.extend(
            accounts
                .into_iter()
                .map(|account| account.map(|a| a.lamports).unwrap_or_default()),
        );
    }

    let mut underfunded = Vec::new();
    let mut total = 0u64;
    for (i, buyer) in cfg.buyers.iter().enumerate() {
        let balance = balances[i];
        let required = buyer.buy_amount_lamports + cfg.funding_buffer_lamports;
        total += balance;
        if balance < required {
            log::warn!(
                "wallet #{i} {} underfunded: has {} lamports, needs {}",
                buyer.keypair.pubkey(),
                balance,
                required
            );
            underfunded.push(i);
        }
    }

    log::info!(
        "wallet preflight: {} wallets, {} lamports total, {} underfunded",
        cfg.buyers.len(),
        total,
        underfunded.len()
    );
    Ok(underfunded)
}
