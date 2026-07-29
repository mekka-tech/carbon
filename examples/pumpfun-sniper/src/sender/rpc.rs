use {
    solana_client::{
        nonblocking::rpc_client::RpcClient,
        rpc_config::{RpcSendTransactionConfig, RpcSimulateTransactionConfig},
    },
    solana_commitment_config::CommitmentConfig,
    solana_transaction::versioned::VersionedTransaction,
};

/// Fire every transaction independently with skip_preflight — a failed
/// preflight would cost the block we're racing for.
pub async fn spray(rpc: &RpcClient, txs: &[VersionedTransaction]) -> Result<(), String> {
    let config = RpcSendTransactionConfig {
        skip_preflight: true,
        preflight_commitment: Some(CommitmentConfig::processed().commitment),
        ..Default::default()
    };
    let sends = txs
        .iter()
        .map(|tx| rpc.send_transaction_with_config(tx, config));
    for result in futures::future::join_all(sends).await {
        match result {
            Ok(signature) => log::info!("sent buy: {signature}"),
            Err(err) => log::error!("send failed: {err}"),
        }
    }
    Ok(())
}

/// Dry-run mode: simulate each transaction and log the outcome.
pub async fn simulate_all(rpc: &RpcClient, txs: &[VersionedTransaction]) -> Result<(), String> {
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
                    None => log::info!(
                        "SIMULATE buyer #{i}: OK (units={:?})",
                        result.units_consumed
                    ),
                    Some(err) => log::error!(
                        "SIMULATE buyer #{i}: FAILED {err:?}\nlogs: {:#?}",
                        result.logs
                    ),
                }
            }
            Err(err) => log::error!("SIMULATE buyer #{i}: rpc error {err}"),
        }
    }
    Ok(())
}
