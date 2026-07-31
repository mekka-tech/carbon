use {
    crate::{
        config::{Buyer, Config, SendPath},
        fills::{Fill, FillRecorder, FillState},
        processor::{Launch, SnipeSignal},
        pump::{
            instructions::{self, CoinAccounts, CoinAccountsV2, StaticAccounts},
            quote::{self, CurveState},
        },
        sender::{fast::FastSenderPool, jito, rpc::RpcPool, tpu::TpuSender},
    },
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_hash::Hash,
    solana_instruction::Instruction,
    solana_message::{v0, VersionedMessage},
    solana_signature::Signature,
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::{collections::HashMap, sync::Arc},
    tokio::sync::{mpsc, RwLock},
};

pub struct BuyDispatcher {
    cfg: Arc<Config>,
    rpc: Arc<RpcPool>,
    fast: Arc<FastSenderPool>,
    tpu: Arc<TpuSender>,
    blockhash: Arc<RwLock<Hash>>,
    statics: Arc<StaticAccounts>,
    initial_curve: CurveState,
    /// v2 curves open against `Global.initial_virtual_quote_reserves` rather
    /// than the native-SOL reserve, so the two flows quote off different
    /// starting states.
    initial_curve_v2: CurveState,
    /// Purchase tracking. Recording is fire-and-forget by construction — see
    /// `fills` — so nothing on this path can be delayed or failed by it.
    fills: FillRecorder,
}

impl BuyDispatcher {
    #[allow(clippy::too_many_arguments)] // wiring constructor, all of it needed
    pub fn new(
        cfg: Arc<Config>,
        rpc: Arc<RpcPool>,
        fast: Arc<FastSenderPool>,
        tpu: Arc<TpuSender>,
        blockhash: Arc<RwLock<Hash>>,
        statics: Arc<StaticAccounts>,
        initial_curve: CurveState,
        initial_curve_v2: CurveState,
        fills: FillRecorder,
    ) -> Self {
        Self {
            cfg,
            rpc,
            fast,
            tpu,
            blockhash,
            statics,
            initial_curve,
            initial_curve_v2,
            fills,
        }
    }

    /// One task per signal.
    ///
    /// This used to await `dispatch` inline, which meant a snipe did not return
    /// until `landing_check` had finished polling — 900ms before its first poll
    /// and up to 3.6s when nothing lands — plus the retry rebuild and the
    /// position write. Any second launch arriving inside that window sat in the
    /// channel behind it. The signal was never dropped, but a create delivered
    /// seconds late is a create missed, and two watched creators launching
    /// together is exactly the case worth being ready for.
    ///
    /// Dispatches for different mints are independent, so running them
    /// concurrently is correct as well as faster.
    pub async fn run(self: Arc<Self>, mut signals: mpsc::Receiver<SnipeSignal>) {
        while let Some(signal) = signals.recv().await {
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                me.dispatch(&signal).await;
            });
        }
    }

    /// Publish every signature that went on the wire to the fill log.
    ///
    /// Synchronous and infallible on purpose: this sits between signing and
    /// sending, and the tracking requirement is that it can never delay or
    /// fail a buy. `FillRecorder::sent` is an unbounded-channel send that
    /// discards its own error, so the worst case here is untracked history.
    fn record_sent(&self, sent: &[SentTx], mint: solana_pubkey::Pubkey) {
        for tx in sent {
            let Some(buyer) = self.cfg.buyers.get(tx.buyer) else {
                continue;
            };
            self.fills.sent(Fill {
                buyer: tx.buyer,
                wallet: buyer.keypair.pubkey(),
                mint,
                signature: tx.signature,
                attempt: tx.attempt,
                state: FillState::Sent,
                slot: None,
                delta: None,
                lamports: tx.lamports,
            });
        }
    }

    async fn dispatch(&self, signal: &SnipeSignal) {
        // v1 and v2 coins need different account sets and different buy
        // instructions; build whichever this launch calls for once, then share
        // it across every buyer.
        let coin = Arc::new(match signal.launch {
            Launch::V1 => Coin::V1(CoinAccounts::new(
                signal.mint,
                signal.bonding_curve,
                signal.associated_bonding_curve,
                &signal.creator,
            )),
            Launch::V2 { mayhem } => Coin::V2(CoinAccountsV2::new(
                signal.mint,
                signal.bonding_curve,
                &signal.creator,
                mayhem,
            )),
        });
        let blockhash = *self.blockhash.read().await;

        // Build and sign all buys in parallel — with 30 wallets, signing
        // serially would add avoidable milliseconds to the hot path.
        let jito_enabled = self.cfg.send_paths.contains(&SendPath::Jito);
        let buyer_count = self.cfg.buyers.len();
        // A zero-amount buy is still a real transaction: it is signed, sprayed
        // on every route, pays base fee, priority fee and tips, creates the
        // ATAs, and is then rejected by the program (BuyZeroAmount, 6020). In
        // Balance mode a wallet too thin to cover its own reserve is sized to
        // zero on purpose, so this is reachable by design rather than by
        // accident, and it must not reach the wire.
        let active: Vec<usize> = (0..buyer_count)
            .filter(|i| {
                self.cfg
                    .buyers
                    .get(*i)
                    .is_some_and(|b| b.buy_amount_lamports() > 0)
            })
            .collect();
        if active.len() < buyer_count {
            log::warn!(
                "[{}] {} of {buyer_count} wallet(s) have a zero buy size and are skipped — \
                 they cannot cover their own fee reserve",
                signal.mint,
                buyer_count.saturating_sub(active.len())
            );
        }
        if active.is_empty() {
            log::error!(
                "[{}] every wallet has a zero buy size — nothing sent. Re-fund the wallets.",
                signal.mint
            );
            return;
        }
        let build_handles: Vec<_> = active
            .iter()
            .copied()
            .map(|i| {
                let cfg = Arc::clone(&self.cfg);
                let statics = Arc::clone(&self.statics);
                let coin = Arc::clone(&coin);
                let curve = if signal.launch.is_v2() {
                    self.initial_curve_v2
                } else {
                    self.initial_curve
                };
                let dev_buy = signal.dev_buy_lamports;
                let tip = (jito_enabled && is_bundle_tail(i, buyer_count))
                    .then_some(self.cfg.jito_tip_lamports);
                tokio::task::spawn_blocking(move || {
                    let buyer = &cfg.buyers[i];
                    build_buy_tx(
                        &cfg,
                        &statics,
                        &coin,
                        buyer,
                        buyer.buy_amount_lamports(),
                        &curve,
                        dev_buy,
                        blockhash,
                        tip,
                    )
                })
            })
            .collect();

        let mut txs = Vec::with_capacity(build_handles.len());
        // Provider assignment must follow the surviving transactions, since a
        // failed build shifts every later index.
        let mut assignments = Vec::with_capacity(build_handles.len());
        // The buyer index has to travel with the transaction for the same
        // reason: a report that reads `buyer #n` off the position in `txs`
        // names the wrong wallet from the first failed build onwards.
        let mut buyer_indices = Vec::with_capacity(build_handles.len());
        // Parallel to `buyer_indices`: the size each surviving transaction was
        // actually built with, captured before any refresh can change it.
        let mut built_amounts: Vec<u64> = Vec::with_capacity(build_handles.len());
        for (slot, handle) in build_handles.into_iter().enumerate() {
            // `active` may be sparse, so the enumerate position is not the
            // buyer index. Reading `cfg.buyers[slot]` here would attribute the
            // provider and the fill record to the wrong wallet.
            let i = active.get(slot).copied().unwrap_or(slot);
            match handle.await {
                Ok(Ok(tx)) => {
                    txs.push(tx);
                    assignments.push(self.cfg.buyers[i].provider);
                    buyer_indices.push(i);
                    built_amounts.push(self.cfg.buyers[i].buy_amount_lamports());
                }
                Ok(Err(err)) => log::error!("[{}] buyer #{i} build failed: {err}", signal.mint),
                Err(err) => log::error!("[{}] buyer #{i} build panicked: {err}", signal.mint),
            }
        }
        if txs.is_empty() {
            log::error!("[{}] no transactions built, nothing to send", signal.mint);
            return;
        }

        log::info!(
            "[{}] {} ({}): dispatching {} buys via {:?} (dev_buy={} lamports)",
            signal.mint,
            signal.name,
            signal.symbol,
            txs.len(),
            self.cfg.send_paths,
            signal.dev_buy_lamports,
        );

        if self.cfg.dry_run {
            crate::sender::rpc::simulate_all(self.rpc.primary(), &txs).await;
            return;
        }

        // Every enabled path sends every transaction, concurrently. Duplicate
        // delivery is safe: a signature can land at most once on-chain, so the
        // fastest route wins and the others are no-ops.
        let mut paths: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
            Vec::new();
        // TPU first. `join_all` polls in index order and every path does its
        // serialization synchronously before its first await, so whichever is
        // pushed first gets its bytes on the wire first. TPU is the only
        // single-round-trip route, so it should never be queued behind three
        // HTTP clients building request bodies.
        if self.cfg.send_paths.contains(&SendPath::Tpu) {
            let tpu = Arc::clone(&self.tpu);
            let txs = txs.clone();
            paths.push(Box::pin(async move { tpu.send(&txs).await }));
        }
        if self.cfg.send_paths.contains(&SendPath::Rpc) {
            let rpc = Arc::clone(&self.rpc);
            let txs = txs.clone();
            paths.push(Box::pin(async move { rpc.spray(&txs).await }));
        }
        if self.cfg.send_paths.contains(&SendPath::Fast) {
            let fast = Arc::clone(&self.fast);
            let txs = txs.clone();
            let assignments = assignments.clone();
            paths.push(Box::pin(async move {
                fast.send_assigned(&txs, &assignments).await
            }));
        }
        if jito_enabled {
            let urls = self.cfg.jito_block_engine_urls.clone();
            let txs = txs.clone();
            paths.push(Box::pin(async move {
                jito::send_bundles(&urls, &txs).await;
            }));
        }
        futures::future::join_all(paths).await;

        // Retry. A snipe that lands nothing is worth another attempt with a
        // fresh blockhash — the usual causes (stale blockhash, a dropped send)
        // are transient. Only retried when *zero* buys landed: a partial fill is
        // a success, and resending would double the position. Each attempt pays
        // priority fees again, so this is bounded by SNIPE_RETRIES.
        //
        // Every signature that goes on the wire is kept, across every attempt:
        // an attempt-1 transaction can still land while attempt 2 is in flight,
        // so dropping the earlier batch would hide a fill from both the landing
        // check and the position record.
        let mut sent: Vec<SentTx> = collect_sent(&txs, &buyer_indices, 0, &built_amounts);
        self.record_sent(&sent, signal.mint);
        for attempt in 1..=self.cfg.snipe_retries {
            match landing_check(self.rpc.primary(), &sent).await {
                LandingCheck::Landed => break,
                // "nothing landed" and "we could not find out" are different
                // facts. N buys x every send path makes an RPC 429 likely, and
                // resending on a blind poll doubles a position that is already
                // open — at whatever price the curve has moved to.
                LandingCheck::Indeterminate => {
                    log::warn!(
                        "[{}] landing status unknown (RPC did not answer) — not retrying",
                        signal.mint
                    );
                    break;
                }
                LandingCheck::NotLanded => {}
            }
            let blockhash = *self.blockhash.read().await;
            log::warn!(
                "[{}] nothing landed — retry {attempt}/{}",
                signal.mint,
                self.cfg.snipe_retries
            );
            let mut retry_txs = Vec::with_capacity(buyer_count);
            let mut retry_assignments = Vec::with_capacity(buyer_count);
            let mut retry_buyers = Vec::with_capacity(buyer_count);
            let mut retry_amounts: Vec<u64> = Vec::with_capacity(buyer_count);
            for (i, buyer) in self.cfg.buyers.iter().enumerate() {
                // Same zero guard as the first pass. A refresh may have zeroed
                // a wallet between attempts, and a 0-lamport retry pays fees
                // for a transaction the program rejects outright.
                if buyer.buy_amount_lamports() == 0 {
                    continue;
                }
                let curve = if signal.launch.is_v2() {
                    self.initial_curve_v2
                } else {
                    self.initial_curve
                };
                // The retry is a fresh set of bundles and needs its own tips.
                // Resending untipped leaves the block engine no reason to
                // include them, so the retry pays fees for nothing.
                let tip = (jito_enabled && is_bundle_tail(i, buyer_count))
                    .then_some(self.cfg.jito_tip_lamports);
                match build_buy_tx(
                    &self.cfg,
                    &self.statics,
                    &coin,
                    buyer,
                    buyer.buy_amount_lamports(),
                    &curve,
                    signal.dev_buy_lamports,
                    blockhash,
                    tip,
                ) {
                    Ok(tx) => {
                        retry_txs.push(tx);
                        retry_assignments.push(buyer.provider);
                        retry_buyers.push(i);
                        retry_amounts.push(buyer.buy_amount_lamports());
                    }
                    Err(err) => log::error!("[{}] retry build #{i}: {err}", signal.mint),
                }
            }
            if retry_txs.is_empty() {
                break;
            }
            let mut retry_paths: Vec<
                std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
            > = Vec::new();
            if self.cfg.send_paths.contains(&SendPath::Rpc) {
                let rpc = Arc::clone(&self.rpc);
                let t = retry_txs.clone();
                retry_paths.push(Box::pin(async move { rpc.spray(&t).await }));
            }
            if self.cfg.send_paths.contains(&SendPath::Fast) {
                let fast = Arc::clone(&self.fast);
                let t = retry_txs.clone();
                let a = retry_assignments.clone();
                retry_paths.push(Box::pin(async move { fast.send_assigned(&t, &a).await }));
            }
            if jito_enabled {
                let urls = self.cfg.jito_block_engine_urls.clone();
                let t = retry_txs.clone();
                retry_paths.push(Box::pin(async move {
                    jito::send_bundles(&urls, &t).await;
                }));
            }
            if self.cfg.send_paths.contains(&SendPath::Tpu) {
                let tpu = Arc::clone(&self.tpu);
                let t = retry_txs.clone();
                retry_paths.push(Box::pin(async move { tpu.send(&t).await }));
            }
            if retry_paths.is_empty() {
                // Nothing to await: `join_all([])` is instant, so the whole
                // retry budget would spin through in one pass while the buys
                // never leave the process.
                log::error!("[{}] retry has no usable send path — stopping", signal.mint);
                break;
            }
            futures::future::join_all(retry_paths).await;
            let retried = collect_sent(&retry_txs, &retry_buyers, attempt, &retry_amounts);
            self.record_sent(&retried, signal.mint);
            sent.extend(retried);
        }

        // Position record. The sniper fires once per mint and then stops, so
        // the exit is a separate step (`sell_all`). It needs the coin identity
        // and the buy signatures to derive cost basis from chain rather than
        // trusting a running tally.
        // The filesystem calls are the async variants on purpose: a blocking
        // `write` on the dispatch task stalls every other snipe sharing the
        // executor thread.
        {
            let dir = std::path::Path::new("positions");
            if let Err(err) = tokio::fs::create_dir_all(dir).await {
                log::warn!("could not create positions dir: {err}");
            } else {
                let record = serde_json::json!({
                    "mint": signal.mint.to_string(),
                    "creator": signal.creator.to_string(),
                    "bonding_curve": signal.bonding_curve.to_string(),
                    "launch_v2": signal.launch.is_v2(),
                    "create_slot": signal.create_slot,
                    "create_signature": signal.create_signature,
                    "dev_buy_lamports": signal.dev_buy_lamports,
                    // Every attempt, not just the first: after a retry it is the
                    // retry transactions that landed, and cost basis is read
                    // back from exactly this list. Signatures that never landed
                    // are skipped when it is read, so listing them is free.
                    "buy_signatures": sent
                        .iter()
                        .map(|s| s.signature.to_string())
                        .collect::<Vec<_>>(),
                });
                let path = dir.join(format!("{}.json", signal.mint));
                match serde_json::to_string_pretty(&record) {
                    Ok(body) => {
                        if let Err(err) = tokio::fs::write(&path, body).await {
                            log::warn!("could not write position {}: {err}", path.display());
                        } else {
                            log::info!("[{}] position recorded -> {}", signal.mint, path.display());
                        }
                    }
                    Err(err) => log::warn!("could not serialise position: {err}"),
                }
            }
        }

        // Landing report. The only exact answer to "which block did we get" is
        // the slot our buy landed in versus the slot the create landed in;
        // wall-clock timestamps cannot resolve ~400ms slots. Runs detached so
        // it never delays the next snipe.
        let rpc = Arc::clone(self.rpc.primary());
        let create_slot = signal.create_slot;
        let mint = signal.mint;
        let fills = self.fills.clone();
        let cfg = Arc::clone(&self.cfg);
        tokio::spawn(async move {
            report_landing(rpc, sent, create_slot, mint, fills, cfg).await;
        });
    }
}

/// A buy that went on the wire, tagged with the wallet it belongs to. The buyer
/// index is carried rather than inferred from a position, because a failed
/// build shifts every later slot and a retry adds a second transaction for the
/// same wallet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SentTx {
    buyer: usize,
    /// 0 for the first dispatch, then the retry number.
    attempt: u32,
    signature: Signature,
    /// Lamports actually built into this transaction.
    ///
    /// Carried rather than re-read from the buyer: sizes are now re-derived
    /// after every confirmed snipe and sell, so a later read returns the NEXT
    /// launch's size and the fill log would attribute a spend that never
    /// happened.
    lamports: u64,
}

/// Pair each built transaction with the buyer it was built for.
fn collect_sent(
    txs: &[VersionedTransaction],
    buyer_indices: &[usize],
    attempt: u32,
    amounts: &[u64],
) -> Vec<SentTx> {
    txs.iter()
        .zip(buyer_indices)
        .zip(amounts)
        .filter_map(|((tx, &buyer), &lamports)| {
            tx.signatures.first().map(|&signature| SentTx {
                buyer,
                attempt,
                signature,
                lamports,
            })
        })
        .collect()
}

/// What one signature's status says about the buy behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TxOutcome {
    /// On chain and executed — tokens were bought.
    Succeeded,
    /// On chain but reverted (a v2 slippage 6042, say). Nothing was bought and
    /// the signature is spent, so this transaction can never land again.
    Failed,
    /// Not on chain, as far as the RPC can see.
    Pending,
}

/// What a round of polling could establish about a whole batch of buys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LandingCheck {
    /// At least one buy is on chain and succeeded.
    Landed,
    /// The RPC answered and no buy has succeeded.
    NotLanded,
    /// The RPC gave no usable answer, so nothing is known either way.
    Indeterminate,
}

/// Fold one poll into the running verdict.
///
/// `poll` is `None` when the RPC call itself failed. That is not evidence that
/// nothing landed — it is no evidence at all, and collapsing the two is what
/// makes a retry fire on top of buys that are already filled. Once something
/// has succeeded the verdict is final; otherwise the newest poll wins, so an
/// RPC that stops answering half way leaves the batch indeterminate rather
/// than falsely clear.
fn fold_poll(current: LandingCheck, poll: Option<&[TxOutcome]>) -> LandingCheck {
    if current == LandingCheck::Landed {
        return current;
    }
    match poll {
        None => LandingCheck::Indeterminate,
        Some(outcomes) if outcomes.contains(&TxOutcome::Succeeded) => LandingCheck::Landed,
        // A reverted buy holds no position, so it must not suppress the retry
        // the way a mere "status exists" check would.
        Some(_) => LandingCheck::NotLanded,
    }
}

/// Have any of these buys landed? Polled briefly — long enough for a send to
/// confirm, short enough that a retry still has a live blockhash.
async fn landing_check(
    rpc: &Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    sent: &[SentTx],
) -> LandingCheck {
    if sent.is_empty() {
        return LandingCheck::NotLanded;
    }
    let signatures: Vec<Signature> = sent.iter().map(|s| s.signature).collect();
    let mut verdict = LandingCheck::Indeterminate;
    for _ in 0..4 {
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
        let poll = rpc
            .get_signature_statuses(&signatures)
            .await
            .ok()
            .map(|statuses| {
                statuses
                    .value
                    .iter()
                    .map(|status| match status {
                        None => TxOutcome::Pending,
                        Some(status) if status.err.is_none() => TxOutcome::Succeeded,
                        Some(_) => TxOutcome::Failed,
                    })
                    .collect::<Vec<_>>()
            });
        verdict = fold_poll(verdict, poll.as_deref());
        if verdict == LandingCheck::Landed {
            break;
        }
    }
    verdict
}

/// Slot distance from the create. Saturating because this runs in a detached
/// task, where an out-of-range slot from a bad RPC answer would otherwise take
/// the whole landing report down without a trace.
fn slot_delta(slot: u64, create_slot: u64) -> i64 {
    if slot >= create_slot {
        i64::try_from(slot.saturating_sub(create_slot)).unwrap_or(i64::MAX)
    } else {
        i64::try_from(create_slot.saturating_sub(slot))
            .map(i64::saturating_neg)
            .unwrap_or(i64::MIN)
    }
}

/// Poll for our buys and report the slot each landed in, relative to the
/// create's slot. `delta=0` is block 0 (same block as the create), `+1` the
/// next block, and so on.
async fn report_landing(
    rpc: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    sent: Vec<SentTx>,
    create_slot: u64,
    mint: solana_pubkey::Pubkey,
    fills: FillRecorder,
    // Re-derive buy sizes once the buys have settled. Balances only change
    // when something lands, so this is driven by confirmation rather than by a
    // timer: no polling when nothing happened, and no window in which the next
    // launch is sized against SOL that has already been spent.
    cfg: Arc<Config>,
) {
    if sent.is_empty() {
        return;
    }
    let signatures: Vec<Signature> = sent.iter().map(|s| s.signature).collect();
    // Keyed by index into `sent`, so the buyer and attempt stay attached.
    let mut resolved: HashMap<usize, (u64, TxOutcome)> = HashMap::new();
    // ~30s: a transaction that has not landed by then never will, since the
    // blockhash it was signed against expires after ~150 slots.
    for _ in 0..15 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let Ok(statuses) = rpc.get_signature_statuses(&signatures).await else {
            continue;
        };
        for (i, status) in statuses.value.iter().enumerate() {
            // An RPC that answers with more statuses than we asked about would
            // index past `sent`; the panic happens inside a detached task, so
            // the report would simply stop existing with nothing logged.
            if i >= sent.len() {
                break;
            }
            if let Some(status) = status {
                let outcome = if status.err.is_none() {
                    TxOutcome::Succeeded
                } else {
                    TxOutcome::Failed
                };
                resolved.entry(i).or_insert((status.slot, outcome));
            }
        }
        if resolved.len() == sent.len() {
            break;
        }
    }

    if resolved.is_empty() {
        log::error!(
            "[{mint}] LANDING: none of {} buys landed within 30s (create_slot={create_slot})",
            sent.len()
        );
        // The blockhash has expired by now, so these can never land. Say so,
        // rather than leaving them reading `sent` forever — a wallet stuck on
        // `sent` is one an operator cannot tell from a slow confirmation.
        for tx in &sent {
            fills.resolved(tx.signature, FillState::Unknown, None, None);
        }
        // Refresh anyway: some fees were still paid, and a wallet that dropped
        // below its reserve must be sized to zero before the next launch.
        crate::wallets::refresh_buy_sizes(&cfg, &rpc).await;
        return;
    }
    let mut deltas: Vec<i64> = Vec::new();
    let mut reverted: usize = 0;
    for (i, (slot, outcome)) in resolved.iter() {
        let Some(tx) = sent.get(*i) else { continue };
        let delta = slot_delta(*slot, create_slot);
        fills.resolved(
            tx.signature,
            if *outcome == TxOutcome::Succeeded {
                FillState::Landed
            } else {
                FillState::Reverted
            },
            Some(*slot),
            Some(delta),
        );
        if *outcome == TxOutcome::Succeeded {
            deltas.push(delta);
            log::info!(
                "[{mint}] LANDED buyer #{}: slot={slot} create_slot={create_slot} delta={delta:+} \
                 ({}) attempt={} sig={}",
                tx.buyer,
                if delta == 0 {
                    "BLOCK 0 — same block as create"
                } else {
                    "block +N"
                },
                tx.attempt,
                tx.signature
            );
        } else {
            // On chain but reverted: fees paid, no tokens. Reported apart from
            // the fills so a failed snipe is never read as a filled one.
            reverted = reverted.saturating_add(1);
            log::error!(
                "[{mint}] REVERTED buyer #{}: slot={slot} create_slot={create_slot} \
                 delta={delta:+} attempt={} sig={}",
                tx.buyer,
                tx.attempt,
                tx.signature
            );
        }
    }
    if deltas.is_empty() {
        log::error!(
            "[{mint}] LANDING SUMMARY: 0/{} filled — {reverted} reverted on chain",
            sent.len()
        );
        crate::wallets::refresh_buy_sizes(&cfg, &rpc).await;
        return;
    }
    let best = deltas.iter().copied().min().unwrap_or_default();
    log::info!(
        "[{mint}] LANDING SUMMARY: {}/{} filled, {reverted} reverted, best delta={best:+} ({})",
        deltas.len(),
        sent.len(),
        if best == 0 {
            "BLOCK 0"
        } else {
            "missed block 0"
        }
    );

    // Buys have landed, so the wallets are near-empty. Re-derive before the
    // next launch can be dispatched against stale amounts.
    crate::wallets::refresh_buy_sizes(&cfg, &rpc).await;
}

/// What one wallet is asked to spend on a manual buy, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuySize {
    pub buyer: usize,
    pub balance: u64,
    pub reserve: u64,
    /// Lamports that will actually be spent on the curve.
    pub spend: u64,
}

/// Lamports a wallet must keep back to pay for its own buy.
///
/// This is the "leaves for gas" part of `b 100`. A buy that spends the entire
/// balance cannot pay its own fees and fails on the spot, so the reserve is
/// computed from what this transaction will actually cost rather than guessed:
///
/// * priority fee — `price × CU_limit / 1e6`, and at the configured
///   55.5M µL/CU × 180k CU this is ~0.01 SOL, the dominant term by far
/// * base fee — 5,000 lamports per signature
/// * two token accounts — the base ATA and, on v2, the WSOL account, at
///   rent-exemption for a token account each
/// * a margin, because the fee config can move between sizing and landing
///
/// Overestimating costs the operator a little unspent SOL. Underestimating
/// costs them the buy. It rounds up.
pub fn gas_reserve(cfg: &Config, buyer: &Buyer) -> u64 {
    /// Rent-exemption for one SPL token account (165 bytes), plus headroom for
    /// the larger Token-2022 layout.
    const TOKEN_ACCOUNT_RENT: u64 = 2_500_000;
    const BASE_FEE: u64 = 5_000;
    const MARGIN: u64 = 2_000_000;
    let priority = u64::from(cfg.compute_unit_limit)
        .saturating_mul(buyer.priority_fee_micro_lamports)
        .saturating_div(1_000_000);
    let tip = buyer
        .provider
        .and_then(|i| cfg.fast_providers.get(i))
        .map_or(0, |p| p.tip_lamports)
        .saturating_add(cfg.jito_tip_lamports);
    priority
        .saturating_add(BASE_FEE)
        .saturating_add(TOKEN_ACCOUNT_RENT.saturating_mul(2))
        .saturating_add(tip)
        .saturating_add(MARGIN)
}

/// Size a manual buy for one wallet: `pct` percent of its balance, capped so
/// the reserve survives.
///
/// At `pct = 100` this is the whole balance minus the reserve — the behaviour
/// `b 100` is named for. Below 100 the percentage is taken against the full
/// balance and then capped, so `b 50` on a wallet with plenty of SOL really is
/// half of it, and only a wallet too thin to cover its own fees gets trimmed.
pub fn size_buy(balance: u64, reserve: u64, pct: f64) -> u64 {
    if !pct.is_finite() || pct <= 0.0 {
        return 0;
    }
    let headroom = balance.saturating_sub(reserve);
    // f64 has 53 bits of mantissa and lamport balances here are far below that,
    // so this is exact for any plausible balance.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    let requested = ((balance as f64) * (pct / 100.0)) as u64;
    requested.min(headroom)
}

/// Buy an already-known coin from chosen wallets, outside the snipe path.
///
/// This is the console's `b` command. It shares `build_buy_tx` with the sniper
/// so the account layout, the v2 wrap/sync/unwrap sequence and the
/// `min_tokens_out` decision cannot drift between a sniped buy and a manual
/// one — a second implementation of a 27-account instruction is a second thing
/// to get wrong.
///
/// Returns the signature per wallet that was sent. `dry_run` simulates instead.
#[allow(clippy::too_many_arguments)]
pub async fn manual_buy(
    cfg: &Config,
    statics: &StaticAccounts,
    rpc: &Arc<RpcPool>,
    fast: &Arc<FastSenderPool>,
    coin: &Coin,
    curve: &CurveState,
    sizes: &[BuySize],
    blockhash: Hash,
    execute: bool,
) -> Vec<(usize, Result<Signature, String>)> {
    let mut txs: Vec<VersionedTransaction> = Vec::new();
    let mut assignments: Vec<Option<usize>> = Vec::new();
    let mut out: Vec<(usize, Result<Signature, String>)> = Vec::new();
    for size in sizes {
        let Some(buyer) = cfg.buyers.get(size.buyer) else {
            out.push((size.buyer, Err("no such wallet".into())));
            continue;
        };
        // A manual buy is not racing a launch, so there is no dev buy to model
        // and no bundle to tip: the coin already exists and the operator is
        // adding to a position at leisure.
        match build_buy_tx(cfg, statics, coin, buyer, size.spend, curve, 0, blockhash, None) {
            Ok(tx) => {
                match tx.signatures.first() {
                    Some(sig) => out.push((size.buyer, Ok(*sig))),
                    // Unreachable via `try_new`, which signs before returning,
                    // but reporting a send with no signature would leave the
                    // operator unable to check whether it landed.
                    None => out.push((size.buyer, Err("built tx carries no signature".into()))),
                }
                txs.push(tx);
                assignments.push(buyer.provider);
            }
            Err(err) => out.push((size.buyer, Err(err))),
        }
    }
    if txs.is_empty() {
        return out;
    }
    if !execute {
        crate::sender::rpc::simulate_all(rpc.primary(), &txs).await;
        return out;
    }
    // Same duplicate-delivery reasoning as the snipe path: a signature lands at
    // most once, so every enabled route sends every transaction.
    let mut paths: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
        Vec::new();
    if cfg.send_paths.contains(&SendPath::Rpc) {
        let (rpc, txs) = (Arc::clone(rpc), txs.clone());
        paths.push(Box::pin(async move { rpc.spray(&txs).await }));
    }
    if cfg.send_paths.contains(&SendPath::Fast) {
        let (fast, txs, assignments) = (Arc::clone(fast), txs.clone(), assignments.clone());
        paths.push(Box::pin(async move {
            fast.send_assigned(&txs, &assignments).await
        }));
    }
    futures::future::join_all(paths).await;
    out
}

/// Jito bundles cap at `MAX_BUNDLE_SIZE` txs, so 30 buys become 6 bundles; the
/// tip rides on the last transaction of each bundle, and on the final buy so
/// that a short trailing bundle is tipped too.
fn is_bundle_tail(i: usize, count: usize) -> bool {
    i % jito::MAX_BUNDLE_SIZE == jito::MAX_BUNDLE_SIZE.saturating_sub(1)
        || i == count.saturating_sub(1)
}

/// The per-coin accounts for whichever launch flow created this coin.
pub enum Coin {
    V1(CoinAccounts),
    V2(CoinAccountsV2),
}

#[allow(clippy::too_many_arguments)]
fn build_buy_tx(
    cfg: &Config,
    statics: &StaticAccounts,
    coin: &Coin,
    buyer: &Buyer,
    // Lamports to spend. Taken as a parameter rather than read off
    // `buyer.buy_amount_lamports` so a manual `b <pct>` from the console can
    // size the buy against the wallet's live balance without fabricating a
    // `Buyer` (whose `Keypair` is not `Clone`) just to carry one number.
    amount_lamports: u64,
    curve: &CurveState,
    dev_buy_lamports: u64,
    blockhash: Hash,
    jito_tip_lamports: Option<u64>,
) -> Result<VersionedTransaction, String> {
    let buyer_pk = buyer.keypair.pubkey();
    // v1 quotes model the curve from `Global`, which carries the v1 opening
    // reserves, and are verified against mainnet.
    //
    // v2 cannot be modelled the same way: `Global` has
    // `initial_virtual_quote_reserves` but NO v2 token reserve — that is set
    // per-coin on the bonding curve at create time. Quoting v2 off the v1 token
    // reserve overstates the output by ~5.8x, and the program rejects the buy
    // with BuySlippageBelowMinTokensOut (6042). Observed on-chain:
    //   demanded 18,481,886,844,622 / available 3,367,478,214,920.
    //
    // Until the reserve is read from the curve, a v2 buy asks for the minimum
    // the program will accept. `min_tokens_out` is slippage protection, and at
    // block 0-1 on a launch we are racing there is nothing to be protected
    // from — a wrong-but-high floor only guarantees the buy fails. Set
    // V2_TRUST_QUOTE=true to re-enable the (currently wrong) modelled floor.
    let min_tokens_out = if matches!(coin, Coin::V2(_)) && !cfg.v2_trust_quote {
        1
    } else {
        quote::min_tokens_out(
            curve,
            amount_lamports,
            dev_buy_lamports,
            cfg.slippage_bps,
        )
    };

    let mut ixs: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(cfg.compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(buyer.priority_fee_micro_lamports),
    ];
    match coin {
        Coin::V1(coin) => ixs.extend([
            instructions::create_ata_idempotent(&buyer_pk, &buyer_pk, &coin.mint),
            instructions::buy_exact_sol_in(
                statics,
                coin,
                &buyer_pk,
                amount_lamports,
                min_tokens_out,
                cfg.track_volume,
            ),
        ]),
        // A v2 buy spends *quote tokens*, so the lamports have to be wrapped
        // first: create the WSOL account, fund it, sync it so the program can
        // see the balance, then buy. The base ATA is Token-2022.
        Coin::V2(coin) => {
            let quote_ata = coin.quote_ata(&buyer_pk);
            ixs.extend([
                instructions::create_ata_idempotent_with_program(
                    &buyer_pk,
                    &buyer_pk,
                    &coin.quote_mint,
                    &coin.quote_token_program,
                ),
                solana_system_interface::instruction::transfer(
                    &buyer_pk,
                    &quote_ata,
                    amount_lamports,
                ),
                instructions::sync_native(&quote_ata),
                instructions::create_ata_idempotent_with_program(
                    &buyer_pk,
                    &buyer_pk,
                    &coin.base_mint,
                    &coin.base_token_program,
                ),
                instructions::buy_exact_quote_in_v2(
                    statics,
                    coin,
                    &buyer_pk,
                    amount_lamports,
                    min_tokens_out,
                ),
            ]);
            // Reclaim whatever the curve did not take, plus the account rent.
            if cfg.unwrap_after_buy {
                ixs.push(instructions::close_account(
                    &quote_ata, &buyer_pk, &buyer_pk,
                ));
            }
        }
    }
    // A fast-provider tip is part of the transaction, so it is baked in for the
    // provider this wallet was dealt. The same transaction still goes out over
    // plain RPC too — the tip only costs anything if this transaction is the
    // one that lands, and it can land at most once.
    if let Some(provider) = buyer.provider.and_then(|i| cfg.fast_providers.get(i)) {
        if let Some(tip_ix) = provider.tip_instruction(&buyer_pk) {
            ixs.push(tip_ix);
        }
    }
    if let Some(tip) = jito_tip_lamports {
        ixs.push(jito::tip_instruction(&buyer_pk, tip));
    }

    let message = v0::Message::try_compile(&buyer_pk, &ixs, &[], blockhash)
        .map_err(|e| format!("compile message: {e}"))?;
    VersionedTransaction::try_new(VersionedMessage::V0(message), &[&buyer.keypair])
        .map_err(|e| format!("sign tx: {e}"))
}

#[cfg(test)]
mod buy_sizing_tests {
    use super::size_buy;

    #[test]
    fn a_full_buy_leaves_the_reserve_behind() {
        // The behaviour `b 100` is named for: spend everything except what the
        // transaction needs to pay for itself.
        let spend = size_buy(1_000_000_000, 15_000_000, 100.0);
        assert_eq!(spend, 985_000_000);
    }

    #[test]
    fn a_partial_buy_is_a_fraction_of_the_balance() {
        // Not a fraction of the headroom — `b 50` on a healthy wallet is half
        // its SOL, and the reserve only binds when the wallet is thin.
        assert_eq!(size_buy(1_000_000_000, 15_000_000, 50.0), 500_000_000);
    }

    #[test]
    fn the_reserve_caps_a_partial_buy_on_a_thin_wallet() {
        // 90% of 20,000,000 is 18,000,000, but only 5,000,000 is spendable.
        assert_eq!(size_buy(20_000_000, 15_000_000, 90.0), 5_000_000);
    }

    #[test]
    fn a_wallet_that_cannot_cover_its_fees_buys_nothing() {
        // Zero, never a wrapped-around huge number: `saturating_sub` is what
        // makes an underfunded wallet skip rather than build a buy that fails
        // after paying priority fees.
        assert_eq!(size_buy(10_000_000, 15_000_000, 100.0), 0);
        assert_eq!(size_buy(0, 15_000_000, 100.0), 0);
    }

    #[test]
    fn a_nonsensical_percentage_buys_nothing() {
        for pct in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            assert_eq!(size_buy(1_000_000_000, 1_000, pct), 0, "pct={pct}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_error_is_not_nothing_landed() {
        // The bug this guards: four failed polls used to read as `false`, and
        // the retry fired on top of buys that were already on chain.
        assert_eq!(
            fold_poll(LandingCheck::Indeterminate, None),
            LandingCheck::Indeterminate
        );
        assert_eq!(
            fold_poll(LandingCheck::NotLanded, None),
            LandingCheck::Indeterminate
        );
    }

    #[test]
    fn reverted_tx_does_not_count_as_landed() {
        let poll = [TxOutcome::Failed, TxOutcome::Pending];
        assert_eq!(
            fold_poll(LandingCheck::Indeterminate, Some(&poll)),
            LandingCheck::NotLanded
        );
    }

    #[test]
    fn one_success_is_enough_and_is_final() {
        let poll = [TxOutcome::Failed, TxOutcome::Succeeded];
        assert_eq!(
            fold_poll(LandingCheck::NotLanded, Some(&poll)),
            LandingCheck::Landed
        );
        // A later RPC failure must not undo a confirmed fill.
        assert_eq!(fold_poll(LandingCheck::Landed, None), LandingCheck::Landed);
        let pending = [TxOutcome::Pending];
        assert_eq!(
            fold_poll(LandingCheck::Landed, Some(&pending)),
            LandingCheck::Landed
        );
    }

    #[test]
    fn all_pending_is_a_definite_not_landed() {
        let poll = [TxOutcome::Pending, TxOutcome::Pending];
        assert_eq!(
            fold_poll(LandingCheck::Indeterminate, Some(&poll)),
            LandingCheck::NotLanded
        );
    }

    #[test]
    fn empty_poll_response_is_not_landed() {
        assert_eq!(
            fold_poll(LandingCheck::Indeterminate, Some(&[])),
            LandingCheck::NotLanded
        );
    }

    #[test]
    fn bundle_tail_is_every_fifth_buy_and_the_last() {
        let count = 12;
        let tails: Vec<usize> = (0..count).filter(|i| is_bundle_tail(*i, count)).collect();
        assert_eq!(tails, vec![4, 9, 11]);
        // A single buy is its own bundle tail.
        assert!(is_bundle_tail(0, 1));
        // Never panics on a degenerate count.
        assert!(is_bundle_tail(0, 0));
    }

    #[test]
    fn slot_delta_saturates_instead_of_wrapping() {
        assert_eq!(slot_delta(100, 100), 0);
        assert_eq!(slot_delta(101, 100), 1);
        assert_eq!(slot_delta(98, 100), -2);
        assert_eq!(slot_delta(u64::MAX, 0), i64::MAX);
        assert_eq!(slot_delta(0, u64::MAX), i64::MIN);
    }
}
