pub mod alt;

use {
    alt::AltCache,
    async_trait::async_trait,
    carbon_core::{
        datasource::{Datasource, DatasourceId, TransactionUpdate, Update, UpdateType},
        error::CarbonResult,
        metrics::{Counter, Histogram, MetricsRegistry},
    },
    carbon_jito_protos::shredstream::{
        shredstream_proxy_client::ShredstreamProxyClient, SubscribeEntriesRequest,
    },
    futures::{stream::try_unfold, TryStreamExt},
    scc::HashCache,
    solana_client::{nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction},
    solana_entry::entry::Entry,
    solana_transaction_status::TransactionStatusMeta,
    std::{
        sync::{Arc, LazyLock},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
    tokio::sync::mpsc::Sender,
    tokio_util::sync::CancellationToken,
    tonic::{
        metadata::{Ascii, MetadataValue},
        transport::{ClientTlsConfig, Endpoint},
        Request,
    },
};

/// Connect timeout applied when the caller does not set one.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

static ENTRY_PROCESS_TIME_NANOS: LazyLock<Histogram> = LazyLock::new(|| {
    Histogram::new(
        "jito_shredstream_grpc_entry_process_time_nanoseconds",
        "Time to process entry in nanoseconds",
        vec![
            1_000.0,
            10_000.0,
            100_000.0,
            1_000_000.0,
            10_000_000.0,
            100_000_000.0,
            1_000_000_000.0,
        ],
    )
});
static ENTRY_UPDATES_RECEIVED: Counter = Counter::new(
    "jito_shredstream_grpc_entry_updates_received_total",
    "Entry updates received from Jito Shredstream gRPC",
);
static DUPLICATE_ENTRIES: Counter = Counter::new(
    "jito_shredstream_grpc_duplicate_entries_total",
    "Duplicate entries skipped in Jito Shredstream gRPC",
);

fn register_jito_shredstream_metrics() {
    let registry = MetricsRegistry::global();
    registry.register_counter(&ENTRY_UPDATES_RECEIVED);
    registry.register_counter(&DUPLICATE_ENTRIES);
    registry.register_histogram(&ENTRY_PROCESS_TIME_NANOS);
}

#[derive(Clone)]
pub struct JitoShredstreamGrpcClient {
    endpoint: String,
    /// Sent as `x-token` metadata on every request. Hosted proxies
    /// (constant-k Prism and similar) reject unauthenticated streams, and often
    /// do so with a bare `HTTP 204 No Content` rather than a gRPC status — which
    /// surfaces as "malformed header: missing HTTP content-type", not as an
    /// auth error. If you see that, the token is missing or wrong.
    x_token: Option<String>,
    /// Only consulted for `https://` endpoints. Defaults to the platform's
    /// enabled roots, which is sufficient for a normally-chained public
    /// certificate.
    tls_config: Option<ClientTlsConfig>,
    connect_timeout: Option<Duration>,
    /// RPC used to resolve Address Lookup Tables. Shreds carry no metadata, so
    /// without this every v0 transaction that uses an ALT decodes to nothing —
    /// silently, with no error. See `alt` module docs.
    alt_rpc: Option<Arc<RpcClient>>,
    /// When non-empty, only transactions whose static account keys include one
    /// of these programs get ALT resolution. `SubscribeEntriesRequest` carries
    /// no server-side filter, so without this we would fetch lookup tables for
    /// every transaction on the network — hundreds of RPC calls a second.
    programs_of_interest: Vec<solana_pubkey::Pubkey>,
}

impl std::fmt::Debug for JitoShredstreamGrpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // RpcClient is not Debug; report whether resolution is on rather than
        // dropping the derive entirely, since "is ALT resolution enabled" is the
        // first thing worth knowing when nothing decodes.
        f.debug_struct("JitoShredstreamGrpcClient")
            .field("endpoint", &self.endpoint)
            .field("x_token", &self.x_token.as_ref().map(|_| "<set>"))
            .field("alt_resolution", &self.alt_rpc.is_some())
            .field("programs_of_interest", &self.programs_of_interest.len())
            .finish()
    }
}

impl JitoShredstreamGrpcClient {
    /// Unauthenticated client, for a local or otherwise open shredstream proxy.
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            x_token: None,
            tls_config: None,
            connect_timeout: None,
            alt_rpc: None,
            programs_of_interest: Vec::new(),
        }
    }

    /// Client that authenticates with `x-token` metadata.
    pub fn new_with_x_token(endpoint: String, x_token: Option<String>) -> Self {
        Self {
            endpoint,
            x_token,
            tls_config: None,
            connect_timeout: None,
            alt_rpc: None,
            programs_of_interest: Vec::new(),
        }
    }

    /// Override TLS configuration. Needed only for a private CA or client
    /// certificates; public certificates work with the default.
    pub fn with_tls_config(mut self, tls_config: ClientTlsConfig) -> Self {
        self.tls_config = Some(tls_config);
        self
    }

    /// Enable Address Lookup Table resolution.
    ///
    /// **Effectively required for decoding.** Without it, any v0 transaction
    /// using an ALT — which is most current Solana traffic, including every
    /// pump.fun v2 launch — yields instructions whose account lists are
    /// truncated, so decoders return `None` and the pipeline reports 100%
    /// success while producing nothing.
    pub fn with_alt_resolution(mut self, rpc: Arc<RpcClient>) -> Self {
        self.alt_rpc = Some(rpc);
        self
    }

    /// Restrict ALT resolution to transactions touching these programs.
    ///
    /// Shredstream has no server-side filtering, so this is the only way to
    /// avoid resolving lookup tables for the entire network's traffic.
    pub fn with_programs_of_interest(mut self, programs: Vec<solana_pubkey::Pubkey>) -> Self {
        self.programs_of_interest = programs;
        self
    }

    pub fn with_connect_timeout(mut self, connect_timeout: Duration) -> Self {
        self.connect_timeout = Some(connect_timeout);
        self
    }
}

#[async_trait]
impl Datasource for JitoShredstreamGrpcClient {
    async fn consume(
        &self,
        id: DatasourceId,
        sender: Sender<(Update, DatasourceId)>,
        cancellation_token: CancellationToken,
    ) -> CarbonResult<()> {
        register_jito_shredstream_metrics();
        let endpoint = self.endpoint.clone();

        // Built explicitly rather than via `ShredstreamProxyClient::connect`, so
        // that TLS and the `x-token` interceptor can be attached.
        let mut builder = Endpoint::from_shared(endpoint.clone()).map_err(|err| {
            carbon_core::error::Error::FailedToConsumeDatasource(format!(
                "invalid shredstream endpoint {endpoint}: {err}"
            ))
        })?;
        if endpoint.starts_with("https://") {
            let tls = self
                .tls_config
                .clone()
                .unwrap_or_else(|| ClientTlsConfig::new().with_enabled_roots());
            builder = builder.tls_config(tls).map_err(|err| {
                carbon_core::error::Error::FailedToConsumeDatasource(format!(
                    "shredstream tls config: {err}"
                ))
            })?;
        }
        let channel = builder
            .connect_timeout(self.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT))
            .connect()
            .await
            .map_err(|err| {
                carbon_core::error::Error::FailedToConsumeDatasource(format!(
                    "failed to connect to shredstream at {endpoint}: {err}"
                ))
            })?;

        // Parsed once here so a malformed token fails at startup rather than on
        // every request.
        let token: Option<MetadataValue<Ascii>> = match self.x_token.as_deref() {
            Some(raw) => Some(raw.parse().map_err(|_| {
                carbon_core::error::Error::FailedToConsumeDatasource(
                    "x-token is not valid ASCII metadata".to_string(),
                )
            })?),
            None => None,
        };
        let mut client =
            ShredstreamProxyClient::with_interceptor(channel, move |mut req: Request<()>| {
                if let Some(token) = token.clone() {
                    req.metadata_mut().insert("x-token", token);
                }
                Ok(req)
            });

        let alt_cache = self.alt_rpc.clone().map(AltCache::new);
        let programs_of_interest = Arc::new(self.programs_of_interest.clone());
        if alt_cache.is_none() {
            log::warn!(
                "shredstream: ALT resolution DISABLED. Every v0 transaction using an address \
                 lookup table will decode to nothing, silently — including all pump.fun v2 \
                 launches. Call with_alt_resolution() unless you only care about legacy transactions."
            );
        }

        tokio::spawn(async move {
            // Reconnect forever.
            //
            // Without this loop the subscription is one-shot, and all three of
            // its endings are permanent: a subscribe failure returns, a stream
            // error returns, and a CLEAN close (`Ok(None)`) falls out of
            // `try_for_each_concurrent` as `Ok` and logs nothing whatsoever.
            //
            // That last one is the dangerous case. When shredstream is the only
            // datasource its sender is the only one, so dropping it closes the
            // pipeline channel and the pipeline shuts down — while an
            // interactive console keeps rendering `*** LIVE ***` and the watch
            // list off RPC. The operator sees an armed sniper that is deaf.
            //
            // Backoff is capped and reset on a successful subscribe: a tight
            // reconnect spin against a rate-limiting endpoint turns a transient
            // outage into an IP ban, which is a permanent one.
            let mut backoff_ms: u64 = 200;
            const MAX_BACKOFF_MS: u64 = 10_000;
            loop {
            if cancellation_token.is_cancelled() {
                log::info!("Cancelling Jito Shreadstream gRPC subscription.");
                return;
            }
            let result = tokio::select! {
                _ = cancellation_token.cancelled() => {
                    log::info!("Cancelling Jito Shreadstream gRPC subscription.");
                    return;
                }

                result = client.subscribe_entries(SubscribeEntriesRequest {}) =>
                    result
            };

            let stream = match result {
                Ok(r) => {
                    log::info!("Jito shredstream subscribed.");
                    backoff_ms = 200;
                    r.into_inner()
                }
                Err(e) => {
                    log::error!("shredstream subscribe failed, retrying in {backoff_ms}ms: {e:?}");
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    backoff_ms = backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS);
                    continue;
                }
            };

            let stream = try_unfold(
                (stream, cancellation_token.clone()),
                |(mut stream, cancellation_token)| async move {
                    tokio::select! {
                        _ = cancellation_token.cancelled() => {
                            log::info!("Cancelling Jito Shreadstream gRPC subscription.");
                            Ok(None)
                        },
                        v = stream.message() => match v {
                            Ok(Some(v)) => Ok(Some((v, (stream, cancellation_token)))),
                            Ok(None) => Ok(None),
                            Err(e) => Err(e),
                        },
                    }
                },
            );

            let dedup_cache = Arc::new(HashCache::with_capacity(1024, 4096));

            if let Err(e) = stream
                .try_for_each_concurrent(None, |message| {
                    let sender = sender.clone();
                    let dedup_cache = dedup_cache.clone();
                    let id_for_closure = id.clone();
                    let alt_cache = alt_cache.clone();
                    let programs_of_interest = programs_of_interest.clone();

                    async move {
                        let start_time = Instant::now();
                        let recv_time = SystemTime::now();
                        let block_time =
                            Some(recv_time.duration_since(UNIX_EPOCH).expect("Time").as_millis() as i64);

                        let entries: Vec<Entry> = match bincode::deserialize(&message.entries) {
                            Ok(e) => e,
                            Err(e) => {
                                log::error!("Failed to deserialize entries at slot {}: {e:?}", message.slot);
                                return Ok(());
                            }
                        };

                        let total_entries = entries.len();
                        let mut duplicate_entries = 0;

                        for entry in entries {
                            if dedup_cache.contains(&entry.hash) {
                                duplicate_entries += 1;
                                continue;
                            }
                            let _ = dedup_cache.put(entry.hash, ());

                            for transaction in entry.transactions {
                                let signature = *transaction.get_signature();

                                // Resolve address lookup tables before moving
                                // the transaction into the update. Without
                                // this, ALT-using transactions produce
                                // truncated account lists and decode to
                                // nothing without raising an error.
                                // Shredstream has NO server-side filter, so this
                                // stream carries every transaction on the network —
                                // votes included, and at mainnet rates that is
                                // thousands per second. Each one that reaches the
                                // pipeline is deep-cloned several times before the
                                // decoder gets to reject it on program id, on a
                                // single-threaded loop. The create we are racing
                                // queues behind all of it.
                                //
                                // So the filter has to happen HERE, before the
                                // channel, not downstream. Sanitized v0 messages
                                // require program_id_index to point into the static
                                // key range, so a pump transaction cannot hide its
                                // program id inside a lookup table — testing the
                                // static keys is sound and needs no ALT resolution.
                                let interesting = programs_of_interest.is_empty()
                                    || transaction
                                        .message
                                        .static_account_keys()
                                        .iter()
                                        .any(|k| programs_of_interest.contains(k));
                                if !interesting {
                                    continue;
                                }

                                let loaded_addresses = match alt_cache.as_ref() {
                                    Some(cache) => cache.resolve(&transaction.message).await,
                                    None => Default::default(),
                                };

                                let update = Update::Transaction(Box::new(TransactionUpdate {
                                    signature,
                                    is_vote: false,
                                    transaction,
                                    meta: TransactionStatusMeta {
                                        status: Ok(()),
                                        loaded_addresses,
                                        ..Default::default()
                                    },
                                    slot: message.slot,
                                    index: None,
                                    block_time,
                                    block_hash: None,
                                }));

                                if let Err(e) = sender.try_send((update, id_for_closure.clone())) {
                                    // `continue`, not `return`: returning here
                                    // abandons every remaining transaction in this
                                    // entry AND every remaining entry in the batch,
                                    // so one full-channel moment drops creates in
                                    // bulk rather than one at a time.
                                    log::error!("Failed to send transaction update with signature {:?} at slot {}: {:?}", signature, message.slot, e);
                                    continue;
                                }
                            }
                        }

                        ENTRY_PROCESS_TIME_NANOS.record(start_time.elapsed().as_nanos() as f64);
                        ENTRY_UPDATES_RECEIVED.inc_by(total_entries as u64);
                        DUPLICATE_ENTRIES.inc_by(duplicate_entries);

                        Ok(())
                    }
                })
                .await
            {
                log::error!("shredstream stream error: {e:?}");
            } else {
                // The silent ending. A clean server-side close is indistinguish-
                // able from healthy completion here, so it MUST be logged: it is
                // the difference between a sniper that is watching and one that
                // only looks like it.
                log::error!(
                    "shredstream closed cleanly — the feed has stopped delivering. Reconnecting."
                );
            }
            if cancellation_token.is_cancelled() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS);
            }
        });

        Ok(())
    }

    fn update_types(&self) -> Vec<UpdateType> {
        vec![UpdateType::Transaction]
    }
}
