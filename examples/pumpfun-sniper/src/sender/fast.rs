//! Anti-MEV / low-latency transaction submission providers.
//!
//! Providers in this category (Helius Sender, Jito's `sendTransaction`,
//! Nextblock, 0slot, Temporal, Astralane, Blockrazor, …) all follow the same
//! shape: a JSON-RPC `sendTransaction` endpoint that expects a base64
//! transaction which includes a SOL tip transfer to one of the provider's tip
//! accounts. So rather than hardcoding one, they are described in config and
//! driven through a single implementation.
//!
//! Tips are per-provider and are baked into the transaction at build time, so
//! each buyer wallet is assigned to one provider (see `Buyer::provider`). The
//! resulting transaction is still *also* sprayed to the plain RPC pool: a tip
//! only costs anything if that transaction lands, and it can land at most once.

use {
    base64::Engine, solana_instruction::Instruction, solana_pubkey::Pubkey,
    solana_system_interface::instruction as system_instruction,
    solana_transaction::versioned::VersionedTransaction, std::sync::Arc,
};

/// Request body shape a provider expects. Most speak JSON-RPC
/// `sendTransaction`; some tip-based landing services take a bare payload
/// instead. Confirm against the provider's own docs — guessing wrong here
/// means silently dropped transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    /// `{"jsonrpc":"2.0","method":"sendTransaction","params":[<base64>, {...}]}`
    JsonRpc,
    /// `{"transaction": "<base64>"}`
    TransactionField,
}

impl std::str::FromStr for PayloadFormat {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "" | "jsonrpc" => Ok(Self::JsonRpc),
            "transaction" => Ok(Self::TransactionField),
            other => Err(format!(
                "payload format must be jsonrpc|transaction, got {other}"
            )),
        }
    }
}

/// A configured fast-send endpoint.
#[derive(Debug, Clone)]
pub struct FastProvider {
    /// Display name, e.g. "helius-fra".
    pub name: String,
    /// Full JSON-RPC endpoint, e.g.
    /// `https://fra-sender.helius-rpc.com/fast` or
    /// `https://frankfurt.mainnet.block-engine.jito.wtf/api/v1/transactions`.
    pub url: String,
    /// Tip accounts to rotate across. Empty means the provider needs no tip.
    pub tip_accounts: Vec<Pubkey>,
    /// Tip size in lamports. Providers enforce their own minimum (Helius
    /// Sender: 0.001 SOL for full routing, 0.000005 SOL in swqos-only mode).
    pub tip_lamports: u64,
    /// Optional `Authorization` header value for providers that require a key.
    pub auth_header: Option<String>,
    /// Request body shape this endpoint expects.
    pub format: PayloadFormat,
}

impl FastProvider {
    /// Tip instruction for this provider, or `None` if it takes no tip.
    /// The tip account is chosen per payer so concurrent buys don't all
    /// write-lock the same account.
    pub fn tip_instruction(&self, payer: &Pubkey) -> Option<Instruction> {
        if self.tip_accounts.is_empty() || self.tip_lamports == 0 {
            return None;
        }
        let index = payer.as_ref()[0] as usize % self.tip_accounts.len();
        Some(system_instruction::transfer(
            payer,
            &self.tip_accounts[index],
            self.tip_lamports,
        ))
    }
}

pub struct FastSenderPool {
    client: reqwest::Client,
    providers: Vec<Arc<FastProvider>>,
}

impl FastSenderPool {
    pub fn new(providers: Vec<FastProvider>) -> Self {
        Self {
            // Connections are pooled and kept alive so the hot path doesn't
            // pay for a TLS handshake per snipe.
            client: reqwest::Client::builder()
                .pool_idle_timeout(std::time::Duration::from_secs(90))
                .tcp_nodelay(true)
                .build()
                .unwrap_or_default(),
            providers: providers.into_iter().map(Arc::new).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Warm each provider's TLS connection so the first real send doesn't pay
    /// the handshake. Failures are logged and ignored.
    pub async fn warm(&self) {
        let warmups = self.providers.iter().map(|provider| {
            let client = self.client.clone();
            let provider = Arc::clone(provider);
            async move {
                match client.get(&provider.url).send().await {
                    Ok(_) => log::debug!("warmed {}", provider.name),
                    Err(err) => log::debug!("warm {} failed (harmless): {err}", provider.name),
                }
            }
        });
        futures::future::join_all(warmups).await;
    }

    /// Send each transaction to the provider its wallet was assigned to.
    /// `assignments` maps transaction index → provider index.
    pub async fn send_assigned(&self, txs: &[VersionedTransaction], assignments: &[Option<usize>]) {
        let sends = txs
            .iter()
            .enumerate()
            .filter_map(|(i, tx)| {
                let provider = self.providers.get((*assignments.get(i)?)?)?;
                let client = self.client.clone();
                let provider = Arc::clone(provider);
                Some(async move {
                    (
                        i,
                        provider.name.clone(),
                        send_one(&client, &provider, tx).await,
                    )
                })
            })
            .collect::<Vec<_>>();

        if sends.is_empty() {
            return;
        }
        let total = sends.len();
        let mut ok = 0usize;
        for (i, name, result) in futures::future::join_all(sends).await {
            match result {
                Ok(_) => {
                    ok += 1;
                    log::debug!("buyer #{i} sent via {name}");
                }
                Err(err) => log::warn!("buyer #{i} via {name} failed: {err}"),
            }
        }
        log::info!("fast senders: {ok}/{total} accepted");
    }
}

async fn send_one(
    client: &reqwest::Client,
    provider: &FastProvider,
    tx: &VersionedTransaction,
) -> Result<String, String> {
    let encoded = bincode::serialize(tx)
        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
        .map_err(|e| format!("serialize tx: {e}"))?;

    let body = match provider.format {
        PayloadFormat::JsonRpc => serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [encoded, {
                "encoding": "base64",
                "skipPreflight": true,
                "maxRetries": 0,
            }],
        }),
        PayloadFormat::TransactionField => serde_json::json!({
            "transaction": encoded,
            "encoding": "base64",
        }),
    };

    let mut request = client.post(&provider.url).json(&body);
    if let Some(auth) = &provider.auth_header {
        request = request.header("Authorization", auth);
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("request to {}: {e}", provider.url))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_success() {
        Ok(text)
    } else {
        Err(format!("{status}: {text}"))
    }
}
