//! Per-wallet purchase tracking.
//!
//! # Why this cannot stall or kill the sniper
//!
//! Tracking sits on the money path, so it is built to be structurally
//! incapable of interfering with it. Three properties, in order of importance:
//!
//! 1. **The hot path never awaits.** Recording is
//!    [`mpsc::UnboundedSender::send`], which is synchronous and does not block
//!    on a full buffer, because the buffer has no bound. The dispatcher calls
//!    it between building a transaction and sending it and cannot be delayed
//!    by a slow reader, a held lock, or a stopped drain task.
//! 2. **The hot path never fails.** `send` returns `Err` only when the
//!    receiver is gone, and that error is dropped. If the drain task dies the
//!    sniper keeps buying with no tracking — the failure mode is a blank
//!    panel, never a missed launch.
//! 3. **Nothing here can panic.** No indexing, no unwrap, no slicing, and
//!    every arithmetic operation is `saturating_*`. Note this is a property of
//!    the code, NOT of the build: the crate does not deny
//!    `clippy::arithmetic_side_effects`, and release builds wrap silently, so
//!    plain `+`/`-` here would be a real hazard rather than a caught one. A
//!    panic in the drain task would be contained to that task, but there is
//!    nothing to contain.
//!
//! The log is a bounded ring: an unbounded *channel* with a bounded *store*.
//! The channel is unbounded so the producer never waits; the store is bounded
//! so a long session cannot grow memory without limit. Backlog is therefore
//! paid in dropped history, never in latency.

use {
    solana_pubkey::Pubkey,
    solana_signature::Signature,
    std::{collections::HashMap, sync::Arc},
    tokio::sync::{mpsc, RwLock},
};

/// How many fills the log keeps. Four wallets × a few retries × many launches;
/// this is far more history than the panel can show and still trivially small.
const CAPACITY: usize = 512;

/// What is known about one buy attempt.
///
/// `Sent` is not a success and is never rendered as one. The distinction that
/// matters operationally is `Landed` (tokens bought) versus `Reverted` (on
/// chain, failed, signature spent — this attempt can never land) versus
/// `Unknown` (the RPC never gave a usable answer, so nothing is known).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillState {
    Sent,
    Landed,
    Reverted,
    Unknown,
}

impl FillState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Landed => "LANDED",
            Self::Reverted => "reverted",
            Self::Unknown => "unknown",
        }
    }
}

/// One buy attempt by one wallet.
#[derive(Clone, Debug)]
pub struct Fill {
    pub buyer: usize,
    pub wallet: Pubkey,
    pub mint: Pubkey,
    pub signature: Signature,
    /// 0 for the first dispatch, then the retry number.
    pub attempt: u32,
    pub state: FillState,
    /// Slot it landed in, once known.
    pub slot: Option<u64>,
    /// Slots behind the create. `0` is same-block, `1` the next block.
    pub delta: Option<i64>,
    /// Lamports the wallet was asked to spend.
    pub lamports: u64,
}

/// Messages the drain task folds into the log.
#[derive(Clone, Debug)]
pub enum FillEvent {
    /// A transaction went on the wire.
    Sent(Box<Fill>),
    /// Landing check came back for a signature.
    Resolved {
        signature: Signature,
        state: FillState,
        slot: Option<u64>,
        delta: Option<i64>,
    },
}

/// Write handle. Cloneable, cheap, and safe to hold on the hot path.
#[derive(Clone, Debug)]
pub struct FillRecorder(mpsc::UnboundedSender<FillEvent>);

impl FillRecorder {
    /// Record a transaction that has been signed and is about to be sent.
    ///
    /// Deliberately infallible and non-async. See the module note: the return
    /// value of the underlying `send` is discarded because a dead drain task
    /// must degrade tracking, not buying.
    pub fn sent(&self, fill: Fill) {
        let _ = self.0.send(FillEvent::Sent(Box::new(fill)));
    }

    /// Record what a landing check established about a signature.
    pub fn resolved(
        &self,
        signature: Signature,
        state: FillState,
        slot: Option<u64>,
        delta: Option<i64>,
    ) {
        let _ = self.0.send(FillEvent::Resolved {
            signature,
            state,
            slot,
            delta,
        });
    }
}

/// Read handle for the panel and the `fills` command.
#[derive(Clone, Default)]
pub struct FillLog(Arc<RwLock<Vec<Fill>>>);

impl FillLog {
    /// Every fill for one mint, newest first.
    pub async fn for_mint(&self, mint: &Pubkey) -> Vec<Fill> {
        let guard = self.0.read().await;
        guard
            .iter()
            .rev()
            .filter(|f| &f.mint == mint)
            .cloned()
            .collect()
    }

    /// Per-wallet outcome for one mint: did this buyer end up holding?
    ///
    /// A wallet with any `Landed` attempt bought, whatever its other attempts
    /// did. Otherwise the newest attempt's state is what the operator needs to
    /// see — a `Reverted` wallet is a wallet to re-buy, an `Unknown` one is a
    /// wallet to check before acting.
    pub async fn outcome_by_wallet(&self, mint: &Pubkey) -> HashMap<usize, FillState> {
        let guard = self.0.read().await;
        let mut out: HashMap<usize, FillState> = HashMap::new();
        for fill in guard.iter().filter(|f| &f.mint == mint) {
            match out.get(&fill.buyer) {
                Some(FillState::Landed) => {}
                _ => {
                    out.insert(fill.buyer, fill.state);
                }
            }
        }
        out
    }

    /// Buyer indices with no landed fill for this mint — the wallets a retry
    /// would target. `total` is the configured buyer count, so a wallet that
    /// never got as far as sending is included.
    pub async fn missing(&self, mint: &Pubkey, total: usize) -> Vec<usize> {
        let landed = self.outcome_by_wallet(mint).await;
        (0..total)
            .filter(|n| landed.get(n) != Some(&FillState::Landed))
            .collect()
    }
}

/// Create the recorder/log pair and spawn the drain task.
///
/// The drain task owns the only receiver and runs until the process exits or
/// every recorder is dropped.
pub fn start() -> (FillRecorder, FillLog) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let log = FillLog::default();
    let sink = Arc::clone(&log.0);
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let mut guard = sink.write().await;
            apply(&mut guard, event);
        }
    });
    (FillRecorder(tx), log)
}

/// Fold one event into the store. Pure, so the ordering rules are testable
/// without a runtime.
///
/// A `Resolved` for an unknown signature is dropped rather than inserted: a
/// fill with no wallet, mint or amount would render as a blank row, and the
/// only way to reach that state is a `Sent` that was evicted by the ring — in
/// which case the row is history the log has already decided not to keep.
fn apply(store: &mut Vec<Fill>, event: FillEvent) {
    match event {
        FillEvent::Sent(fill) => {
            if store.len() >= CAPACITY {
                store.remove(0);
            }
            store.push(*fill);
        }
        FillEvent::Resolved {
            signature,
            state,
            slot,
            delta,
        } => {
            if let Some(fill) = store.iter_mut().find(|f| f.signature == signature) {
                // `Landed` is terminal. A later poll that reads `Pending` off a
                // lagging RPC replica must not walk a confirmed buy back to
                // unknown — that would show a filled wallet as one to re-buy.
                if fill.state != FillState::Landed {
                    fill.state = state;
                    fill.slot = slot;
                    fill.delta = delta;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::str::FromStr};

    fn fill(buyer: usize, sig_byte: u8) -> Fill {
        Fill {
            buyer,
            wallet: Pubkey::new_unique(),
            mint: Pubkey::from_str("11111111111111111111111111111111").unwrap(),
            signature: Signature::from([sig_byte; 64]),
            attempt: 0,
            state: FillState::Sent,
            slot: None,
            delta: None,
            lamports: 100_000_000,
        }
    }

    #[test]
    fn resolving_updates_the_matching_signature() {
        let mut store = Vec::new();
        apply(&mut store, FillEvent::Sent(Box::new(fill(0, 1))));
        apply(&mut store, FillEvent::Sent(Box::new(fill(1, 2))));
        apply(
            &mut store,
            FillEvent::Resolved {
                signature: Signature::from([2; 64]),
                state: FillState::Landed,
                slot: Some(500),
                delta: Some(1),
            },
        );
        assert_eq!(store[0].state, FillState::Sent);
        assert_eq!(store[1].state, FillState::Landed);
        assert_eq!(store[1].delta, Some(1));
    }

    #[test]
    fn landed_is_terminal() {
        // A lagging replica answering `Pending` after a confirmation must not
        // walk the buy back — that would list a filled wallet as unfilled.
        let mut store = Vec::new();
        apply(&mut store, FillEvent::Sent(Box::new(fill(0, 1))));
        for state in [FillState::Landed, FillState::Unknown, FillState::Reverted] {
            apply(
                &mut store,
                FillEvent::Resolved {
                    signature: Signature::from([1; 64]),
                    state,
                    slot: Some(9),
                    delta: Some(0),
                },
            );
        }
        assert_eq!(store[0].state, FillState::Landed);
        assert_eq!(store[0].slot, Some(9));
    }

    #[test]
    fn resolving_an_unknown_signature_is_dropped() {
        let mut store = Vec::new();
        apply(
            &mut store,
            FillEvent::Resolved {
                signature: Signature::from([7; 64]),
                state: FillState::Landed,
                slot: None,
                delta: None,
            },
        );
        assert!(store.is_empty(), "must not invent a fill with no wallet");
    }

    #[test]
    fn the_ring_is_bounded() {
        let mut store = Vec::new();
        for i in 0..(CAPACITY + 20) {
            let mut f = fill(i % 4, 1);
            f.signature = Signature::from([u8::try_from(i % 251).unwrap_or(0); 64]);
            apply(&mut store, FillEvent::Sent(Box::new(f)));
        }
        assert_eq!(store.len(), CAPACITY);
    }

    #[tokio::test]
    async fn missing_lists_wallets_without_a_landed_buy() {
        let mint = Pubkey::from_str("11111111111111111111111111111111").unwrap();
        let log = FillLog::default();
        {
            let mut guard = log.0.write().await;
            let mut a = fill(0, 1);
            a.state = FillState::Landed;
            let mut b = fill(1, 2);
            b.state = FillState::Reverted;
            apply(&mut guard, FillEvent::Sent(Box::new(a)));
            apply(&mut guard, FillEvent::Sent(Box::new(b)));
        }
        // 0 landed; 1 reverted; 2 and 3 never sent. All but 0 need a buy.
        assert_eq!(log.missing(&mint, 4).await, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn a_landed_attempt_outranks_a_later_reverted_one() {
        // A wallet that filled on attempt 0 and whose attempt-1 retry reverted
        // still holds tokens. Reporting it as failed would prompt a re-buy of a
        // position the operator already has.
        let mint = Pubkey::from_str("11111111111111111111111111111111").unwrap();
        let log = FillLog::default();
        {
            let mut guard = log.0.write().await;
            let mut a = fill(0, 1);
            a.state = FillState::Landed;
            let mut b = fill(0, 2);
            b.attempt = 1;
            b.state = FillState::Reverted;
            apply(&mut guard, FillEvent::Sent(Box::new(a)));
            apply(&mut guard, FillEvent::Sent(Box::new(b)));
        }
        assert_eq!(
            log.outcome_by_wallet(&mint).await.get(&0),
            Some(&FillState::Landed)
        );
        assert!(log.missing(&mint, 1).await.is_empty());
    }

    #[tokio::test]
    async fn recording_survives_a_dead_drain_task() {
        // The guarantee the dispatcher relies on: if the drain task is gone,
        // recording is a no-op, not an error and not a stall.
        let (recorder, _log) = {
            let (tx, rx) = mpsc::unbounded_channel();
            drop(rx);
            (FillRecorder(tx), FillLog::default())
        };
        recorder.sent(fill(0, 1));
        recorder.resolved(Signature::from([1; 64]), FillState::Landed, None, None);
    }
}
