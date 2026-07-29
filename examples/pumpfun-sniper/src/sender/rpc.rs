use {
    solana_client::{
        nonblocking::rpc_client::RpcClient,
        rpc_config::{RpcSendTransactionConfig, RpcSimulateTransactionConfig},
    },
    solana_commitment_config::CommitmentConfig,
    solana_transaction::versioned::VersionedTransaction,
    std::sync::Arc,
};

/// A pool of RPC endpoints. Every transaction goes to every endpoint: the same
/// signature can only be included once on-chain, so duplicating across
/// providers costs nothing and takes the fastest route available.
pub struct RpcPool {
    clients: Vec<Arc<RpcClient>>,
}

impl RpcPool {
    pub fn new(urls: &[String]) -> Self {
        Self {
            clients: urls
                .iter()
                .map(|url| {
                    Arc::new(RpcClient::new_with_commitment(
                        url.clone(),
                        CommitmentConfig::processed(),
                    ))
                })
                .collect(),
        }
    }

    pub fn primary(&self) -> &Arc<RpcClient> {
        &self.clients[0]
    }

    /// Fan every transaction out to every endpoint concurrently. Preflight and
    /// RPC-side retries are both disabled: either would cost us the slot we
    /// are racing for.
    pub async fn spray(&self, txs: &[VersionedTransaction]) {
        let config = RpcSendTransactionConfig {
            skip_preflight: true,
            max_retries: Some(0),
            preflight_commitment: Some(CommitmentConfig::processed().commitment),
            ..Default::default()
        };

        let mut sends = Vec::with_capacity(txs.len() * self.clients.len());
        for (i, tx) in txs.iter().enumerate() {
            for client in &self.clients {
                sends.push(
                    async move { (i, client.send_transaction_with_config(tx, config).await) },
                );
            }
        }

        let mut sent = 0usize;
        let mut failed = 0usize;
        for (i, result) in futures::future::join_all(sends).await {
            match result {
                Ok(signature) => {
                    sent += 1;
                    log::debug!("buyer #{i} sent: {signature}");
                }
                Err(err) => {
                    failed += 1;
                    log::warn!("buyer #{i} send failed: {err}");
                }
            }
        }
        log::info!(
            "rpc spray: {sent} accepted, {failed} failed ({} txs × {} endpoints)",
            txs.len(),
            self.clients.len()
        );
    }
}

/// Dry-run mode: simulate each transaction and log the outcome.
pub async fn simulate_all(rpc: &RpcClient, txs: &[VersionedTransaction]) {
    let config = RpcSimulateTransactionConfig {
        sig_verify: false,
        replace_recent_blockhash: true,
        commitment: Some(CommitmentConfig::processed()),
        ..Default::default()
    };
    for (i, tx) in txs.iter().enumerate() {
        match rpc
            .simulate_transaction_with_config(tx, config.clone())
            .await
        {
            Ok(response) => {
                let result = response.value;
                match result.err {
                    None => {
                        log::info!(
                            "SIMULATE buyer #{i}: OK (units={:?})",
                            result.units_consumed
                        )
                    }
                    Some(err) => {
                        log::error!(
                            "SIMULATE buyer #{i}: FAILED {err:?}\nlogs: {:#?}",
                            result.logs
                        )
                    }
                }
            }
            Err(err) => log::error!("SIMULATE buyer #{i}: rpc error {err}"),
        }
    }
}
