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
/// How long to wait before re-simulating a failed dry-run buy, to let the
/// create we are reacting to actually land on the RPC we simulate against.
const RESIMULATE_DELAY: std::time::Duration = std::time::Duration::from_millis(2500);

pub async fn simulate_all(rpc: &RpcClient, txs: &[VersionedTransaction]) {
    let config = RpcSimulateTransactionConfig {
        sig_verify: false,
        replace_recent_blockhash: true,
        commitment: Some(CommitmentConfig::processed()),
        ..Default::default()
    };
    // Simulated concurrently so a dry run matches the live send paths, which
    // all fan out with join_all. Sequentially awaiting each one spread the
    // buys across seconds and made buyer #0 race the create it was reacting
    // to — an artifact of the simulator, not of dispatch.
    let sims = txs.iter().enumerate().map(|(i, tx)| {
        let config = config.clone();
        async move { (i, rpc.simulate_transaction_with_config(tx, config).await) }
    });
    let mut failed_indexes: Vec<usize> = Vec::new();
    for (i, outcome) in futures::future::join_all(sims).await {
        if matches!(&outcome, Ok(r) if r.value.err.is_some()) {
            failed_indexes.push(i);
        }
        match outcome {
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

    // A dry run reacts to the create the instant it appears on the feed, which
    // is before the RPC we simulate against has processed the block containing
    // it. The mint therefore does not exist yet, and the Token-2022 ATA
    // creation fails with IncorrectProgramId — an artifact of simulating
    // against a bank that is behind us, not a defect in the transaction.
    //
    // Live sending has no equivalent problem: the buy is submitted to a leader
    // and executes after the create in block order. To make the dry run
    // actually validate the transaction, re-simulate the failures once the
    // create has had time to land.
    if !failed_indexes.is_empty() {
        tokio::time::sleep(RESIMULATE_DELAY).await;
        log::info!(
            "SIMULATE: re-running {} failed buy(s) after {:?}, now that the create should have landed",
            failed_indexes.len(),
            RESIMULATE_DELAY
        );
        let retries = failed_indexes.into_iter().map(|i| {
            let config = config.clone();
            let tx = &txs[i];
            async move { (i, rpc.simulate_transaction_with_config(tx, config).await) }
        });
        for (i, outcome) in futures::future::join_all(retries).await {
            match outcome {
                Ok(response) => match response.value.err {
                    None => log::info!(
                        "SIMULATE(retry) buyer #{i}: OK (units={:?}) — transaction is valid; the \
                         first failure was the create not having landed yet",
                        response.value.units_consumed
                    ),
                    Some(err) => log::error!(
                        "SIMULATE(retry) buyer #{i}: STILL FAILED {err:?} — this is a real defect, \
                         not a timing artifact\nlogs: {:#?}",
                        response.value.logs
                    ),
                },
                Err(err) => log::error!("SIMULATE(retry) buyer #{i}: rpc error {err}"),
            }
        }
    }
}
