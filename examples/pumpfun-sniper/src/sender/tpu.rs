//! Direct-to-TPU submission over QUIC.
//!
//! Every other send path hands the transaction to somebody else's server and
//! hopes it forwards it to the right validator in time. This one skips that
//! hop: we track the leader schedule ourselves, resolve the next few leaders'
//! TPU/QUIC sockets, and push the signed transactions straight at them.
//!
//! Two things make that fast enough to be worth it:
//!
//! * **Nothing on the hot path talks to an RPC.** A background task refreshes
//!   the leader schedule and the cluster-node address map; [`TpuSender::send`]
//!   only reads an `Arc<Vec<..>>` snapshot of the already-resolved targets.
//! * **Connections are pre-warmed.** A cold QUIC connection costs a handshake
//!   (1-RTT, plus TLS) before the first byte of the transaction moves, which
//!   would defeat the entire point of the path. The connection cache treats an
//!   *empty* payload as a warm-up: it establishes the connection and returns
//!   without sending a packet, so the real send is a single round trip. The
//!   refresher warms every target on every tick.
//!
//! ## Staking caveat (read before tuning)
//!
//! Agave's TPU applies QUIC stake-weighted admission: a validator reserves most
//! of its connection and stream budget for peers with stake, and unstaked
//! senders share a small residual pool. We connect with a throwaway identity
//! (see [`TpuSender::new`]), which means unstaked treatment: connections can be
//! refused outright under load, and accepted streams are rate limited. Landing
//! improves markedly with a **staked identity** used as the client certificate,
//! or by renting **staked connections** from a provider (Helius Sender / Jito /
//! Nextblock / 0slot all sell exactly this). Until then, treat direct-TPU as an
//! additive latency bet, not as a replacement for the RPC spray.
//!
//! Consequently every failure here is logged and swallowed. The same signed
//! transactions go out over the RPC spray concurrently, and a signature can
//! land at most once on-chain, so a dead TPU path costs nothing but a log line.

use {
    futures::future::join_all,
    solana_client::{
        connection_cache::ConnectionCache, nonblocking::rpc_client::RpcClient,
        rpc_response::RpcContactInfo,
    },
    solana_connection_cache::nonblocking::client_connection::ClientConnection,
    solana_pubkey::Pubkey,
    solana_quic_definitions::QUIC_PORT_OFFSET,
    solana_transaction::versioned::VersionedTransaction,
    std::{
        collections::HashMap,
        net::SocketAddr,
        str::FromStr,
        sync::{Arc, RwLock},
        time::{Duration, Instant},
    },
};

/// Slots a leader owns in a row before the schedule rotates (Agave's
/// `NUM_CONSECUTIVE_LEADER_SLOTS`). Used to size the `getSlotLeaders` window.
const NUM_CONSECUTIVE_LEADER_SLOTS: u64 = 4;

/// `getSlotLeaders` refuses windows larger than this.
const MAX_SLOT_LEADERS: u64 = 5_000;

/// How often the background task re-reads the leader schedule and re-warms.
///
/// A slot is ~400 ms and leaders rotate every four of them, so once per slot
/// tracks rotations with three slots of margin. Each tick costs two RPC calls
/// (`getSlot` + `getSlotLeaders`), i.e. ~5 req/s — cheap for a dedicated
/// endpoint, worth raising if `RPC_URLS` points at a shared/metered one.
const LEADER_REFRESH_INTERVAL: Duration = Duration::from_millis(400);

/// Cluster gossip addresses change on the order of restarts, not slots.
const CLUSTER_NODES_TTL: Duration = Duration::from_secs(60);

/// One connection per leader, deliberately.
///
/// The cache hands out a *random* pool member per `get_*_connection` call, so
/// with a pool larger than one a send can pick a member the warmer never
/// touched and pay the handshake anyway — which is the one thing this path
/// exists to avoid. A single connection multiplexes all of the buys as
/// separate unidirectional QUIC streams.
const CONNECTION_POOL_SIZE: usize = 1;

/// Ceiling on a single leader's send. QUIC has no application-level ack here,
/// so a black-holed peer would otherwise hold the dispatcher's `join_all` open;
/// the slot we are racing for is gone long before that matters.
const SEND_TIMEOUT: Duration = Duration::from_millis(400);

/// Ceiling on one warm-up handshake. Runs off the hot path, so this is only
/// about not letting the refresher wedge.
const WARM_TIMEOUT: Duration = Duration::from_secs(2);

/// An upcoming leader and the QUIC socket its TPU listens on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderTarget {
    pub identity: Pubkey,
    pub addr: SocketAddr,
}

struct NodeCache {
    /// Validator identity → TPU/QUIC socket, from `getClusterNodes`.
    tpu_quic: Arc<HashMap<Pubkey, SocketAddr>>,
    fetched_at: Option<Instant>,
}

pub struct TpuSender {
    rpc: Arc<RpcClient>,
    cache: ConnectionCache,
    /// How many distinct upcoming leaders to target (`TPU_LEADERS_AHEAD`).
    leaders_ahead: usize,
    /// Resolved, deduped, in slot order. Swapped wholesale by the refresher so
    /// the hot path only ever clones an `Arc`.
    targets: RwLock<Arc<Vec<LeaderTarget>>>,
    nodes: RwLock<NodeCache>,
}

impl TpuSender {
    /// Build a sender. Does no I/O: call [`TpuSender::refresh`] once and then
    /// [`TpuSender::spawn_refresher`] to populate and keep targets warm.
    ///
    /// The connection cache generates a throwaway keypair for its QUIC client
    /// certificate, so we present as an unstaked peer (see the module docs). To
    /// use a staked identity instead, swap `new_quic` for
    /// `ConnectionCache::new_with_client_options(name, pool, None,
    /// Some((&identity_keypair, local_ip)), None)`.
    pub fn new(rpc: Arc<RpcClient>, leaders_ahead: u64) -> Self {
        Self {
            rpc,
            cache: ConnectionCache::new_quic("pumpfun-sniper-tpu", CONNECTION_POOL_SIZE),
            leaders_ahead: leaders_ahead as usize,
            targets: RwLock::new(Arc::new(Vec::new())),
            nodes: RwLock::new(NodeCache {
                tpu_quic: Arc::new(HashMap::new()),
                fetched_at: None,
            }),
        }
    }

    /// `TPU_LEADERS_AHEAD=0` disables the path even if `SEND_PATHS` lists it.
    pub fn is_enabled(&self) -> bool {
        self.leaders_ahead > 0
    }

    /// Current targets. Cheap: clones one `Arc`, never awaits, never blocks on
    /// anything but an uncontended read lock.
    pub fn targets(&self) -> Arc<Vec<LeaderTarget>> {
        Arc::clone(&read(&self.targets))
    }

    /// Keep the target set fresh and its connections warm, forever.
    pub fn spawn_refresher(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(LEADER_REFRESH_INTERVAL).await;
                this.refresh().await;
            }
        })
    }

    /// One refresh cycle: cluster nodes (if stale) → current slot → upcoming
    /// leaders → resolved sockets → pre-warm. Every step degrades to "log it
    /// and keep the previous target set" rather than failing.
    pub async fn refresh(&self) {
        if !self.is_enabled() {
            return;
        }

        if self.nodes_are_stale() {
            match self.rpc.get_cluster_nodes().await {
                Ok(nodes) => {
                    let map = build_node_map(&nodes);
                    log::debug!(
                        "tpu: {} of {} cluster nodes published a TPU/QUIC address",
                        map.len(),
                        nodes.len()
                    );
                    let mut guard = write(&self.nodes);
                    guard.tpu_quic = Arc::new(map);
                    guard.fetched_at = Some(Instant::now());
                }
                // Keep whatever map we already had; addresses are stable enough
                // that a stale one still beats no targets at all.
                Err(err) => log::warn!("tpu: getClusterNodes failed: {err}"),
            }
        }

        let nodes = Arc::clone(&read(&self.nodes).tpu_quic);
        if nodes.is_empty() {
            log::warn!(
                "tpu: no cluster-node TPU addresses available yet — some RPC providers strip \
                 getClusterNodes; point RPC_URLS at one that doesn't if the tpu path stays empty"
            );
            return;
        }

        let slot = match self.rpc.get_slot().await {
            Ok(slot) => slot,
            Err(err) => {
                log::warn!("tpu: getSlot failed: {err}");
                return;
            }
        };
        let leaders = match self
            .rpc
            .get_slot_leaders(slot, slots_to_query(self.leaders_ahead))
            .await
        {
            Ok(leaders) => leaders,
            Err(err) => {
                log::warn!("tpu: getSlotLeaders({slot}) failed: {err}");
                return;
            }
        };

        let upcoming = next_distinct_leaders(&leaders, self.leaders_ahead);
        let targets = resolve_targets(&upcoming, &nodes);
        if targets.is_empty() {
            log::warn!(
                "tpu: none of the next {} leader(s) from slot {slot} published a TPU/QUIC address",
                upcoming.len()
            );
            return;
        }

        // Only log on rotation — this runs several times per slot.
        if self.targets().as_slice() != targets.as_slice() {
            log::debug!(
                "tpu: targeting {} leader(s) from slot {slot}: {}",
                targets.len(),
                targets
                    .iter()
                    .map(|t| format!("{}@{}", t.identity, t.addr))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        *write(&self.targets) = Arc::new(targets);

        self.warm().await;
    }

    /// Establish (or confirm) a QUIC connection to every current target
    /// without sending a packet, so the next real send is one round trip.
    pub async fn warm(&self) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let warmups = targets.iter().map(|target| async move {
            // An empty payload is the connection cache's own warm-up signal:
            // it connects (or reuses) and returns before opening a stream.
            let connection = self.cache.get_nonblocking_connection(&target.addr);
            match tokio::time::timeout(WARM_TIMEOUT, connection.send_data(&[])).await {
                Ok(Ok(())) => log::trace!("tpu: warm {} ok", target.addr),
                Ok(Err(err)) => log::debug!("tpu: warm {} failed: {err}", target.addr),
                Err(_) => log::debug!("tpu: warm {} timed out", target.addr),
            }
        });
        join_all(warmups).await;
    }

    /// Fan every transaction at every targeted leader, concurrently.
    ///
    /// Duplicate delivery is free: the same signature can be included at most
    /// once on-chain, so hitting two or three upcoming leaders just buys us a
    /// second and third chance at an early block.
    ///
    /// Note what "accepted" means here: QUIC acknowledges that the stream was
    /// written to the leader, not that the leader's banking stage kept the
    /// transaction. It is a delivery count, not a landing count.
    pub async fn send(&self, txs: &[VersionedTransaction]) {
        if !self.is_enabled() {
            return;
        }
        let targets = self.targets();
        if targets.is_empty() {
            log::warn!("tpu: no leader targets resolved, skipping (rpc spray still covers this)");
            return;
        }

        let mut wire = Vec::with_capacity(txs.len());
        for (i, tx) in txs.iter().enumerate() {
            match bincode::serialize(tx) {
                Ok(bytes) => wire.push(bytes),
                Err(err) => log::error!("tpu: buyer #{i} serialize failed: {err}"),
            }
        }
        if wire.is_empty() {
            return;
        }

        let wire = &wire;
        let sends = targets.iter().map(|target| async move {
            let connection = self.cache.get_nonblocking_connection(&target.addr);
            let result = tokio::time::timeout(SEND_TIMEOUT, connection.send_data_batch(wire)).await;
            (target, result)
        });

        let mut ok = 0usize;
        let mut failed = 0usize;
        for (target, result) in join_all(sends).await {
            match result {
                Ok(Ok(())) => {
                    ok += 1;
                    log::debug!("tpu: delivered to {} ({})", target.addr, target.identity);
                }
                Ok(Err(err)) => {
                    failed += 1;
                    log::warn!("tpu: send to {} failed: {err}", target.addr);
                }
                Err(_) => {
                    failed += 1;
                    log::warn!("tpu: send to {} timed out", target.addr);
                }
            }
        }
        log::info!(
            "tpu direct: {ok} accepted, {failed} failed ({} txs × {} leader(s))",
            wire.len(),
            targets.len()
        );
    }

    fn nodes_are_stale(&self) -> bool {
        read(&self.nodes)
            .fetched_at
            .is_none_or(|at| at.elapsed() >= CLUSTER_NODES_TTL)
    }
}

/// Lock helpers that survive a poisoned lock instead of panicking. Nothing here
/// holds a guard across an await, so a poisoned lock can only mean an unrelated
/// panic unwound through one — the data is still structurally fine.
fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

/// How many slots of leader schedule to pull to be sure we see `leaders_ahead`
/// *distinct* leaders.
///
/// The window starts mid-rotation in the general case (we could be on the last
/// slot of the current leader's four), so covering N further leaders needs
/// N + 1 rotations' worth of slots.
fn slots_to_query(leaders_ahead: usize) -> u64 {
    let leaders = leaders_ahead.max(1) as u64;
    leaders
        .saturating_add(1)
        .saturating_mul(NUM_CONSECUTIVE_LEADER_SLOTS)
        .min(MAX_SLOT_LEADERS)
}

/// First `limit` distinct identities from a slot-ordered leader list, order
/// preserved. Consecutive slots share a leader, and a validator can win two
/// rotations in a row, so plain truncation would target the same node twice.
fn next_distinct_leaders(slot_leaders: &[Pubkey], limit: usize) -> Vec<Pubkey> {
    let mut seen = std::collections::HashSet::with_capacity(limit);
    let mut out = Vec::with_capacity(limit);
    for leader in slot_leaders {
        if out.len() == limit {
            break;
        }
        if seen.insert(*leader) {
            out.push(*leader);
        }
    }
    out
}

/// Map leaders to their TPU/QUIC sockets, dropping identities gossip does not
/// know about and collapsing duplicate sockets (several identities can sit
/// behind one address in shared-infrastructure setups; sending twice down the
/// same connection would only waste stream budget).
fn resolve_targets(leaders: &[Pubkey], nodes: &HashMap<Pubkey, SocketAddr>) -> Vec<LeaderTarget> {
    let mut seen = std::collections::HashSet::with_capacity(leaders.len());
    let mut out = Vec::with_capacity(leaders.len());
    for identity in leaders {
        match nodes.get(identity) {
            Some(addr) if seen.insert(*addr) => out.push(LeaderTarget {
                identity: *identity,
                addr: *addr,
            }),
            Some(_) => {}
            None => log::debug!("tpu: leader {identity} has no gossiped TPU address"),
        }
    }
    out
}

/// Identity → TPU/QUIC socket for every node that advertises one.
fn build_node_map(nodes: &[RpcContactInfo]) -> HashMap<Pubkey, SocketAddr> {
    nodes
        .iter()
        .filter_map(|node| {
            let identity = Pubkey::from_str(&node.pubkey).ok()?;
            Some((identity, tpu_quic_addr(node.tpu_quic, node.tpu)?))
        })
        .collect()
}

/// Prefer the gossiped QUIC socket; fall back to the UDP TPU port plus Agave's
/// fixed QUIC offset, which is how the QUIC port is derived when a node does
/// not advertise it separately.
fn tpu_quic_addr(tpu_quic: Option<SocketAddr>, tpu: Option<SocketAddr>) -> Option<SocketAddr> {
    if let Some(addr) = tpu_quic {
        return Some(addr);
    }
    let tpu = tpu?;
    let port = tpu.port().checked_add(QUIC_PORT_OFFSET)?;
    Some(SocketAddr::new(tpu.ip(), port))
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::net::{IpAddr, Ipv4Addr},
    };

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    fn addr(last_octet: u8, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, last_octet)), port)
    }

    #[test]
    fn slots_to_query_covers_a_partial_rotation() {
        // One leader ahead can still straddle two rotations.
        assert_eq!(slots_to_query(1), 8);
        assert_eq!(slots_to_query(2), 12);
        assert_eq!(slots_to_query(4), 20);
        // 0 is treated as 1 — callers gate on `is_enabled` before this runs.
        assert_eq!(slots_to_query(0), 8);
        // Never exceeds what getSlotLeaders will serve.
        assert_eq!(slots_to_query(usize::MAX), MAX_SLOT_LEADERS);
    }

    #[test]
    fn next_distinct_leaders_collapses_consecutive_slots() {
        let schedule = vec![key(1), key(1), key(1), key(1), key(2), key(2), key(3)];
        assert_eq!(next_distinct_leaders(&schedule, 2), vec![key(1), key(2)]);
        assert_eq!(
            next_distinct_leaders(&schedule, 3),
            vec![key(1), key(2), key(3)]
        );
    }

    #[test]
    fn next_distinct_leaders_handles_a_repeat_rotation() {
        // Same validator wins two rotations in a row with another in between.
        let schedule = vec![key(1), key(1), key(2), key(2), key(1), key(1), key(3)];
        assert_eq!(
            next_distinct_leaders(&schedule, 3),
            vec![key(1), key(2), key(3)]
        );
    }

    #[test]
    fn next_distinct_leaders_tolerates_a_short_schedule() {
        assert!(next_distinct_leaders(&[], 3).is_empty());
        assert_eq!(next_distinct_leaders(&[key(9)], 3), vec![key(9)]);
        assert!(next_distinct_leaders(&[key(9)], 0).is_empty());
    }

    #[test]
    fn resolve_targets_skips_unknown_and_dedupes_addresses() {
        let mut nodes = HashMap::new();
        nodes.insert(key(1), addr(1, 8009));
        nodes.insert(key(2), addr(1, 8009)); // same box, second identity
        nodes.insert(key(3), addr(3, 8009));

        // key(4) is not in gossip at all and must be dropped, not panicked on.
        let targets = resolve_targets(&[key(1), key(2), key(4), key(3)], &nodes);
        assert_eq!(
            targets,
            vec![
                LeaderTarget {
                    identity: key(1),
                    addr: addr(1, 8009)
                },
                LeaderTarget {
                    identity: key(3),
                    addr: addr(3, 8009)
                },
            ]
        );
    }

    #[test]
    fn resolve_targets_preserves_slot_order() {
        let mut nodes = HashMap::new();
        nodes.insert(key(1), addr(1, 8009));
        nodes.insert(key(2), addr(2, 8009));
        let targets = resolve_targets(&[key(2), key(1)], &nodes);
        assert_eq!(
            targets.iter().map(|t| t.addr).collect::<Vec<_>>(),
            vec![addr(2, 8009), addr(1, 8009)]
        );
    }

    #[test]
    fn tpu_quic_addr_prefers_the_gossiped_quic_socket() {
        assert_eq!(
            tpu_quic_addr(Some(addr(1, 8009)), Some(addr(1, 8003))),
            Some(addr(1, 8009))
        );
    }

    #[test]
    fn tpu_quic_addr_falls_back_to_the_udp_port_plus_offset() {
        assert_eq!(
            tpu_quic_addr(None, Some(addr(1, 8003))),
            Some(addr(1, 8003 + QUIC_PORT_OFFSET))
        );
    }

    #[test]
    fn tpu_quic_addr_degrades_instead_of_overflowing() {
        assert_eq!(tpu_quic_addr(None, None), None);
        assert_eq!(tpu_quic_addr(None, Some(addr(1, u16::MAX))), None);
    }

    #[test]
    fn build_node_map_ignores_undecodable_and_addressless_nodes() {
        let node = |pubkey: &str, tpu_quic, tpu| RpcContactInfo {
            pubkey: pubkey.to_string(),
            gossip: None,
            tvu: None,
            tpu,
            tpu_quic,
            tpu_forwards: None,
            tpu_forwards_quic: None,
            tpu_vote: None,
            serve_repair: None,
            rpc: None,
            pubsub: None,
            version: None,
            feature_set: None,
            shred_version: None,
        };
        let good = key(7).to_string();
        let nodes = vec![
            node(&good, Some(addr(7, 8009)), None),
            node("not-a-pubkey", Some(addr(8, 8009)), None),
            node(&key(9).to_string(), None, None),
        ];

        let map = build_node_map(&nodes);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&key(7)), Some(&addr(7, 8009)));
    }
}
