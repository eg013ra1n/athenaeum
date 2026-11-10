//! The live collab fetch engine: [`run_live`] over `store.remote()`.
//!
//! Frames arrive on a channel as [`LiveItem`]s. Each is one raw blob with its
//! own `GetRequest`, hash, [`ProviderSet`] and cancel switch; the run assigns it
//! to a provider, moves the bytes still missing, and sends its [`ItemOutcome`]
//! the moment it ends. One item's failure is its own result and never aborts a
//! sibling. Every round recomputes the missing range from the local store, so a
//! reassignment resumes at the byte and never restarts the frame.
//!
//! ## Providers and back-off
//!
//! Per-provider state is run-wide: one ledger ([`ProviderStats`]), one back-off
//! ladder and one in-flight count per provider, whichever items it serves.
//! [`pick_provider`] is a deliberately dumb least-loaded pick, kept as ONE
//! function so a ranking is one edit. A [`ProviderSet`] may be live: a provider
//! that appears is picked up at the item's next round, and an item with no
//! provider sleeps until one appears.
//!
//! [`EVICT_AFTER_FAILURES`] consecutive failures put a provider in back-off
//! (`BACKOFF_BASE`, doubling per further failure up to `BACKOFF_MAX_RUNGS`
//! rungs). An item that finds every provider parked waits for the earliest and
//! tries again, at most `MAX_BACKOFF_ROUNDS` times. A refusal is read, not
//! struck: `ERR_LIMIT` parks the provider for [`LIMIT_RETRY`], `ERR_PERMISSION`
//! excludes it for that item only, and bytes that fail verification exclude it
//! for the item and strike it like any failure.
//!
//! ## The stall ceiling
//!
//! An assignment whose `bytes_read` has not grown for [`STALL_HARD_LIMIT`] is a
//! provider failure, whatever QUIC thinks: a peer that accepts the connection
//! and then dribbles 20 KB/s never errors, so without this deadline the item
//! would never move off it.
//!
//! ## Hedging
//!
//! While an item's primary transfer runs unhedged it is re-evaluated every
//! [`HEDGE_REEVALUATE_INTERVAL`] against the trigger `max(p95 of recent
//! completions, [`HEDGE_EXPECTED_MULTIPLIER`] × expected)`. Late by either
//! measure arms a hedge on a different provider for the BACK HALF of the item's
//! still-missing range, split at the midpoint ([`split_missing_at_midpoint`]).
//! A hedge is paid from the [`HedgeBudget`], whose cap follows the items in
//! flight: the larger of [`HEDGE_BUDGET_RATIO`] of their bytes and half the
//! largest one, so a run can always afford one hedge of its largest item.
//! Whichever side finishes first wins the round and the loser is cancelled (see
//! Cancellation below). A primary that fails while its hedge is still live does
//! not cancel the hedge: it is a real transfer, so it is promoted and carries
//! the round alone (`primary_gone`). See [`try_arm_hedge`] for the trigger and
//! the budget check.
//!
//! ## Progress truth
//!
//! Nothing here emits UI progress. `store.observe()` is the single progress
//! oracle: the get streams are consumed here only to watch for movement, and
//! the `Progress(u64)` values never leave this module.
//!
//! ## Cancellation
//!
//! Dropping the [`GetProgress`](iroh_blobs::api::remote::GetProgress) stream IS
//! the cancellation, and **no pooled-connection close is needed** — verified by
//! reading the chain rather than assumed:
//!
//! 1. `GetProgress { rx, fut }` holds the `execute_get_sink` future *boxed
//!    inside itself*; `GetProgress::stream()` (`api/remote.rs::into_stream`)
//!    drives that future inside the generator it returns. Dropping the stream
//!    therefore drops the future.
//! 2. `execute_get_sink` opens its own `StreamPair` per request
//!    (`Connection::open_stream_pair` → `open_bi`), so dropping the future
//!    drops that pair's `RecvStream`.
//! 3. `noq-1.3.0`'s `impl Drop for RecvStream` (recv_stream.rs:613) calls
//!    `conn.inner.recv_stream(id).stop(0)` whenever the stream was not read to
//!    completion — a QUIC `STOP_SENDING`, which resets the provider's send side
//!    promptly.
//!
//! The pooled `ConnectionRef` is dropped with it, releasing the pool permit
//! while leaving the connection warm for the next item — which is what we
//! want: a stalled peer is not necessarily a dead one, and closing its
//! connection would make the next attempt pay a fresh handshake.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use iroh::EndpointId;
use iroh_blobs::api::blobs::Blobs;
use iroh_blobs::api::remote::{GetProgressItem, Remote};
use iroh_blobs::api::Store;
use iroh_blobs::get::Stats;
use iroh_blobs::protocol::{ChunkRanges, ChunkRangesExt, GetRequest};
use iroh_blobs::Hash;
use n0_future::StreamExt as _;
use tokio::sync::{mpsc, watch};

use super::collab_pool::{CollabPool, PooledConn};

use crate::sharing::{ProviderEvent, ProviderTelemetrySink};

/// D4 §6: an assignment whose `bytes_read` has not grown for this long is a
/// provider failure, error or no error. Upper bound borrowed from libtorrent's
/// `piece_timeout = 20 s`.
pub(crate) const STALL_HARD_LIMIT: Duration = Duration::from_secs(20);

/// D4 §6: consecutive failures before a provider is put in backoff. Reset on
/// the provider's next success.
pub(crate) const EVICT_AFTER_FAILURES: u32 = 3;

/// D4 §6: first backoff rung; doubles per further failure, capped at
/// [`BACKOFF_MAX_RUNGS`] (the iroh-blobs 0.35 `RetryConfig` shape).
pub(crate) const BACKOFF_BASE: Duration = Duration::from_millis(500);

/// Backoff rungs, i.e. `BACKOFF_BASE * 2^k` for `k` in `0..BACKOFF_MAX_RUNGS`
/// — 500 ms through 16 s.
const BACKOFF_MAX_RUNGS: u32 = 6;

/// How many times ONE child may find every provider in backoff, wait for the
/// earliest of them and try again, before it gives up. With
/// [`BACKOFF_MAX_RUNGS`] this bounds a dead swarm's failure at roughly
/// `0.5 + 1 + 2 + 4 + 8 + 16 = 31.5 s` of waiting plus the dial attempts
/// between the rungs — a bounded ladder, never a spin.
const MAX_BACKOFF_ROUNDS: u32 = 6;

/// Spec §7.2 / T12 ruling R2: a provider that refused a get with `ERR_LIMIT`
/// (at its upload stream limit) is not asked again for this long — for any
/// hash — and gets no failure strike: it is alive and serving others.
pub(crate) const LIMIT_RETRY: Duration = Duration::from_secs(2);

/// How often an item that must stop at a yield checks how far it got.
const YIELD_CUT_POLL: Duration = Duration::from_millis(250);

/// Floor on the sleep a child takes when every provider is in backoff, so a
/// clock that has already crossed `next_try` by a hair cannot spin.
const MIN_BACKOFF_SLEEP: Duration = Duration::from_millis(10);

/// Floor on the watchdog's own wait slice. Without it a `stall_hard_limit`
/// already consumed would ask `timeout` for `Duration::ZERO` and busy-poll the
/// stream.
const MIN_WATCHDOG_SLICE: Duration = Duration::from_millis(50);

/// D4 §4.5: hedging may spend about 5 % extra bytes (the gRPC hedging budget).
pub(crate) const HEDGE_BUDGET_RATIO: f64 = 0.05;

/// D4 §4.5: hedge once an assignment has run longer than
/// `max(p95 of recent completions, this × expected)`. The p95 rule is Dean &
/// Barroso's; the multiplier is Boxo's `MessageLatencyMultiplier`.
pub(crate) const HEDGE_EXPECTED_MULTIPLIER: f64 = 2.0;

/// How many recent completions the p95 is taken over (D4 §4.5).
const HEDGE_COMPLETION_WINDOW: usize = 32;

/// EWMA smoothing for per-provider goodput, with a `min(1/n, α)` warm-up so the
/// first samples are not dragged toward a cold zero (D4 §6 — explicitly
/// "настроечное", a tuning value with no authority behind it).
const GOODPUT_ALPHA: f64 = 0.25;

/// How often an un-hedged assignment re-evaluates whether it may hedge.
///
/// OURS, not the design's: the design sketches one `sleep_until(hedge_at)`, but
/// `hedge_at` cannot be computed until the child's SIZE is known, and the size
/// is only known once bytes have flowed. A refused budget must not disqualify a
/// child permanently either, or one momentary shortfall strands it on a useless
/// peer for the rest of the run. So the arm decision is a poll, not a one-shot.
///
/// One consequence worth knowing before reading a provider's numbers: while
/// this tick runs [`try_arm_hedge`], THE PRIMARY IS NOT POLLED. The `select!`
/// has already committed to this branch, and `blobs.observe(hash).await` inside
/// it is an await on the local store, so a store that is slow to answer parks
/// the primary for exactly as long as it takes. That time still accrues to the
/// primary's wall-clock `elapsed`, and so to its goodput, as though the peer
/// had gone idle — the provider is billed for the store's latency. In practice
/// `observe` answers from memory and this is a footnote; it would become a real
/// bound if it ever went to disk on a cold cache.
const HEDGE_REEVALUATE_INTERVAL: Duration = Duration::from_millis(250);

/// Per-provider transfer facts — what each provider actually moved, for the
/// run's report.
///
/// `bytes` is payload bytes THIS provider delivered: `Stats.payload_bytes_read`
/// on a completed assignment, and the last observed progress value on one that
/// stalled or failed (so a provider that served 90 % and then died is not
/// recorded as having served nothing). `elapsed` is wall clock, dial included —
/// one rule for every outcome; `Stats.elapsed` (transfer only) is available at
/// the [`transfer_once`] seam when A4 wants a tighter goodput denominator.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProviderStats {
    pub bytes: u64,
    pub children: u32,
    pub failures: u32,
    pub elapsed: Duration,
}

/// What one assignment run did, per provider plus the swarm-wide counters.
#[derive(Debug, Clone, Default)]
pub(crate) struct AssignmentReport {
    pub per_provider: HashMap<EndpointId, ProviderStats>,
    pub stalls: u32,
    /// Hedges ARMED during the run.
    pub hedges: u32,
    /// DUPLICATE bytes hedging actually cost: the part of each hedge's charge
    /// that was not refunded, i.e. bytes some peer sent that another peer had
    /// already sent. This is the quantity [`HEDGE_BUDGET_RATIO`] is a budget
    /// FOR, and it is deliberately not "bytes a hedge delivered" — a hedge that
    /// wins delivers the half its primary never would have and duplicates
    /// nothing, so counting its delivery as a cost would libel the mechanism.
    pub hedge_bytes: u64,
}

impl AssignmentReport {
    /// Total payload bytes accounted to some provider.
    pub(crate) fn total_bytes(&self) -> u64 {
        self.per_provider.values().map(|s| s.bytes).sum()
    }

    /// Total items some provider carried to completion (test oracle).
    ///
    /// Report-only: an item already complete at its first round (a resumed
    /// fetch finding it already local) never runs `record_success`, so it is
    /// credited to nobody and this sum undercounts the run's items by exactly
    /// that many.
    #[cfg(test)]
    pub(crate) fn total_children(&self) -> u32 {
        self.per_provider.values().map(|s| s.children).sum()
    }
}

/// The last [`HEDGE_COMPLETION_WINDOW`] assignment durations, for the p95 half
/// of the hedge trigger.
#[derive(Debug, Default)]
pub(crate) struct CompletionWindow(VecDeque<Duration>);

impl CompletionWindow {
    fn record(&mut self, elapsed: Duration) {
        if self.0.len() == HEDGE_COMPLETION_WINDOW {
            self.0.pop_front();
        }
        self.0.push_back(elapsed);
    }

    /// The 95th percentile of the window, or `None` while it is empty.
    ///
    /// Nearest-rank on the sorted sample: with one completion the p95 IS that
    /// completion, which is the honest answer — a single data point cannot say
    /// anything about a tail, and the trigger's other half (2 × expected) is
    /// what carries the early run.
    pub(crate) fn p95(&self) -> Option<Duration> {
        if self.0.is_empty() {
            return None;
        }
        let mut sorted: Vec<Duration> = self.0.iter().copied().collect();
        sorted.sort_unstable();
        let rank = ((sorted.len() as f64) * 0.95).ceil() as usize;
        Some(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
    }
}

/// The gRPC-shaped token bucket that keeps hedging from turning a degraded
/// network into a byte storm (D4 §4.5).
///
/// **It starts FULL.** The design says what the bucket EARNS
/// ([`HEDGE_BUDGET_RATIO`] × the bytes of every completed item) and what it is
/// capped at, but not where it starts; a bucket that starts empty can never pay
/// for the very first hedge, which is the one that matters on a run whose
/// first assignment lands on a useless peer. Starting full is also what a token
/// bucket means everywhere else, and the cap still bounds the total spend.
///
/// **A live run's cap** follows the items IN FLIGHT: the larger of
/// [`HEDGE_BUDGET_RATIO`] × their bytes and half of the largest one — the cost
/// of one hedge of it — and `0` when nothing is in flight. Tokens only come back
/// as a refund of bytes that were never duplicated or as a share of completed
/// bytes, so the duplicate bytes outstanding stay bounded by the cap, at most
/// half the largest item. A full bucket can always pay for one hedge, so the
/// stranded tail of a run is hedged one item at a time instead of waiting out
/// the stall ceiling.
#[derive(Debug)]
pub(crate) struct HedgeBudget {
    tokens: f64,
    cap: f64,
    /// The sizes of the items in flight, with how many of each (a multiset:
    /// equal sizes retire one at a time).
    in_flight: BTreeMap<u64, usize>,
    /// The sum of `in_flight`, counting every item.
    in_flight_bytes: u64,
}

impl HedgeBudget {
    /// An empty live budget: no item in flight, so no cap.
    fn live() -> Self {
        Self {
            tokens: 0.0,
            cap: 0.0,
            in_flight: BTreeMap::new(),
            in_flight_bytes: 0,
        }
    }

    /// A budget with a fixed cap of [`HEDGE_BUDGET_RATIO`] × `total_bytes`, for
    /// the arithmetic tests; `extend`/`retire` replace the fixed cap with the
    /// live one.
    #[cfg(test)]
    fn new(total_bytes: u64) -> Self {
        let cap = HEDGE_BUDGET_RATIO * total_bytes as f64;
        Self {
            tokens: cap,
            cap,
            ..Self::live()
        }
    }

    /// The cap the items in flight call for.
    fn live_cap(&self) -> f64 {
        match self.in_flight.keys().next_back() {
            None => 0.0,
            Some(&largest) => {
                (HEDGE_BUDGET_RATIO * self.in_flight_bytes as f64).max(largest as f64 / 2.0)
            }
        }
    }

    /// A live run's item joins the run: the cap grows to what the items in
    /// flight call for, and the tokens grow by the same amount.
    fn extend(&mut self, bytes: u64) {
        *self.in_flight.entry(bytes).or_insert(0) += 1;
        self.in_flight_bytes = self.in_flight_bytes.saturating_add(bytes);
        let cap = self.live_cap();
        self.tokens += (cap - self.cap).max(0.0);
        self.cap = cap;
    }

    /// A live run's item has ended: the cap falls to what the items still in
    /// flight call for, and the tokens follow it down.
    fn retire(&mut self, bytes: u64) {
        match self.in_flight.get_mut(&bytes) {
            Some(n) if *n > 1 => *n -= 1,
            Some(_) => {
                self.in_flight.remove(&bytes);
            }
            None => {
                debug_assert!(false, "a retired item was taken first");
                tracing::error!(bytes, "hedge budget retired an item it never took");
                return;
            }
        }
        self.in_flight_bytes = self.in_flight_bytes.saturating_sub(bytes);
        self.cap = self.live_cap();
        self.tokens = self.tokens.min(self.cap);
    }

    /// A completed child refills the bucket, never past the cap.
    fn earn(&mut self, bytes: u64) {
        self.tokens = (self.tokens + HEDGE_BUDGET_RATIO * bytes as f64).min(self.cap);
    }

    /// D4 §4.5: hedge only while the bucket is at least half full AND can pay
    /// for this hedge's whole range. The half rule is the storm brake — it
    /// stops hedging long before the budget is actually exhausted, so a
    /// degraded run cannot ride the bucket to zero one hedge at a time.
    ///
    /// The comparison is `>=`, not `>`, and that is load-bearing rather than
    /// cosmetic: tokens land on EXACTLY `cap / 2` in an ordinary case — two
    /// providers splitting the items evenly, where completed bytes are half
    /// the run's bytes and the cap is 5 % of them — and a strict `>` refused every
    /// hedge there for pure arithmetic reasons. Controller ruling R1.
    fn try_charge(&mut self, bytes: u64) -> bool {
        let want = bytes as f64;
        if self.tokens >= self.cap / 2.0 && self.tokens >= want {
            self.tokens -= want;
            true
        } else {
            false
        }
    }

    /// Give back the part of a charge that never became a duplicate byte.
    fn refund(&mut self, bytes: u64) {
        self.tokens = (self.tokens + bytes as f64).min(self.cap);
    }
}

/// The run-wide hedge ledger: one budget, one completion window, and the report
/// counters. Behind one mutex, never held across an await.
#[derive(Debug)]
struct HedgeLedger {
    budget: HedgeBudget,
    completions: CompletionWindow,
    hedges: u32,
    hedge_bytes: u64,
}

type Ledger = Arc<Mutex<HedgeLedger>>;

/// Byte deltas per (item, provider), exact — unlike `ProviderTelemetrySink`
/// this is fed from our own transfer loop, not the downloader's sampled
/// channel. Advisory UI data (spec 2026-09-29 §6.2).
pub(crate) type DeliveredSink = Arc<dyn Fn(&str, [u8; 32], u64) + Send + Sync>;

/// Knobs for one assignment run.
#[derive(Clone)]
pub(crate) struct AssignmentOptions {
    /// D4 §6's hard progress deadline. Production always passes
    /// [`STALL_HARD_LIMIT`]; tests shorten it so a deliberately trickling peer
    /// trips it inside a test's patience.
    pub stall_hard_limit: Duration,
    /// Race a second assignment for the back half of a slow item's missing
    /// range, under [`HedgeBudget`].
    pub hedging: bool,
    /// Per-provider attempt telemetry (advisory).
    pub telemetry: ProviderTelemetrySink,
    /// Fed the exact bytes a transfer just moved, per (item key, provider) —
    /// `None` for every caller that has no receive-feed UI to keep.
    pub delivered: Option<DeliveredSink>,
}

/// One independently fetched blob: its own request, its own provider list,
/// its own result.
///
/// `request` is `GetRequest::blob(hash)` for a raw blob — the engine never
/// builds a request itself, it only narrows this one's ranges for a hedge
/// ([`hedge_back_half_request`]). `hash` is the blob the request lands, which
/// the hedge observes. `size` is the caller's announced size, informational
/// only (`0` when unknown); the live run extends its hedge budget by it
/// ([`HedgeBudget::extend`]).
///
/// `providers` is a [`ProviderSet`], which may be live ([`run_live`]).
pub(crate) struct FetchItem {
    /// The caller's id for the item (a frame uuid), echoed in the results.
    pub key: String,
    pub request: GetRequest,
    pub hash: Hash,
    pub size: u64,
    /// The providers this item may be assigned to. Per-provider state
    /// (backoff, load, goodput) is shared across every item of the call.
    pub providers: ProviderSet,
}

impl FetchItem {
    /// How error messages name the item. A request that is not a raw blob is
    /// named `child {index} of {root}`.
    fn describe(&self) -> String {
        if self.request.ranges.is_blob() {
            format!("blob {} ({})", self.key, self.hash)
        } else {
            format!("child {} of {}", self.key, self.request.hash)
        }
    }
}

/// An item's providers (spec §7.2 "live providers").
///
/// `Live` is a watch channel the
/// collab scheduler feeds from holder and presence events: a provider that
/// appears is picked up at the item's next round, and an item whose set is
/// empty sleeps until one appears (it never fails for waiting). A provider
/// that leaves the set is not assigned new work for the item; a transfer
/// already running on it goes on (I5 — only a closed connection or a failed
/// dial takes a provider out of an in-flight fetch).
///
/// `Fixed` (tests only) is a list frozen for the call.
#[derive(Clone)]
pub(crate) enum ProviderSet {
    #[cfg(test)]
    Fixed(Arc<Vec<EndpointId>>),
    Live(watch::Receiver<Arc<Vec<EndpointId>>>),
}

impl ProviderSet {
    /// The providers right now.
    pub(crate) fn current(&self) -> Arc<Vec<EndpointId>> {
        match self {
            #[cfg(test)]
            Self::Fixed(list) => Arc::clone(list),
            Self::Live(rx) => Arc::clone(&rx.borrow()),
        }
    }

    /// As [`current`](Self::current), marking a live value seen so
    /// [`changed`](Self::changed) waits for the NEXT one.
    fn current_seen(&mut self) -> Arc<Vec<EndpointId>> {
        match self {
            #[cfg(test)]
            Self::Fixed(list) => Arc::clone(list),
            Self::Live(rx) => Arc::clone(&rx.borrow_and_update()),
        }
    }

    /// Wait until the set changes. `false` when it never can: a fixed list,
    /// or a live set whose sender is gone.
    async fn changed(&mut self) -> bool {
        match self {
            #[cfg(test)]
            Self::Fixed(_) => false,
            Self::Live(rx) => rx.changed().await.is_ok(),
        }
    }

    /// Sleep for `wait`, or less if a live set changes first. `true` when
    /// the timer ran out, `false` when a set change woke it early.
    async fn sleep_or_change(&mut self, wait: Duration) -> bool {
        match self {
            #[cfg(test)]
            Self::Fixed(_) => {
                tokio::time::sleep(wait).await;
                true
            }
            Self::Live(rx) => {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => true,
                    changed = rx.changed() => {
                        if changed.is_err() {
                            // The sender is gone: the set is final, so only
                            // the backoff can free a provider now.
                            tokio::time::sleep(wait).await;
                            true
                        } else {
                            false
                        }
                    }
                }
            }
        }
    }

    fn is_live(&self) -> bool {
        matches!(self, Self::Live(_))
    }
}

/// A live item whose providers ALL refused it (or served bytes that failed
/// verification) ends with this error — the scheduler owns when to try it
/// again (several refusal causes are transient: storage unmounted, a
/// mismatch cleared by a rehash, a provider still landing its own copy).
/// Recognise it with [`is_refused_by_every_provider`]; its message starts
/// with "refused by every provider".
#[derive(Debug)]
pub(crate) struct RefusedByEveryProvider {
    pub item: String,
}

impl std::fmt::Display for RefusedByEveryProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "refused by every provider: {}", self.item)
    }
}

impl std::error::Error for RefusedByEveryProvider {}

/// Whether a live item failed because every provider refused it.
pub(crate) fn is_refused_by_every_provider(e: &anyhow::Error) -> bool {
    e.downcast_ref::<RefusedByEveryProvider>().is_some()
}

/// A live item's "transferring" flag (`cut_at_yield` reads it): set the
/// moment a provider is claimed, cleared ONLY where the item starts to wait
/// — for a provider to appear, or on parked/busy providers (Task 15, T12
/// minor). A round that ends in a hedge win or a fault and goes straight
/// into the next round stays "transferring", so a yield never cuts an item
/// whose blob is about to be complete (the hedge-win window).
fn set_transferring(flag: &AtomicBool, on: bool) {
    flag.store(on, Ordering::Release);
}

/// How the live run finds a provider's dial address (the scheduler's holder
/// map and presence); `None` means no address is known.
pub(crate) type ProviderAddrs =
    Arc<dyn Fn(&EndpointId) -> Option<iroh::EndpointAddr> + Send + Sync>;

/// How transfers get their connection.
///
/// The dedicated collab pool (spec §7.3): one kept-open connection per
/// provider, dialled with its own transport config at the address `addrs`
/// gives.
#[derive(Clone)]
pub(crate) struct Dialer {
    pub(crate) pool: Arc<CollabPool>,
    pub(crate) addrs: ProviderAddrs,
}

/// One frame handed to [`run_live`], with its own cancel switch: `true`
/// stops that item alone (a new version, an exclusion, a lost project —
/// spec §7.4), keeping its verified bytes in the store.
pub(crate) struct LiveItem {
    pub item: FetchItem,
    pub cancel: watch::Receiver<bool>,
}

/// How one live item ended, sent on [`run_live`]'s `done` channel the
/// moment it ends.
#[derive(Debug)]
pub(crate) enum ItemOutcome {
    /// Complete and verified in the store.
    Done,
    /// Cancelled — its switch, or cut at a yield (an item that was not
    /// transferring ends at once; one that was, once it moved the work-unit
    /// cap). Its verified partial bytes stay in the store, and the scheduler
    /// re-queues it: a resumed fetch asks only for what is still missing.
    Cancelled,
    /// The item exhausted its ladder or hit an error no provider owns.
    Failed(anyhow::Error),
}

/// What the live run learned about a provider, for the scheduler's
/// per-provider health (spec §7.2, §7.3). None of these is a failed fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiveVerdict {
    /// `ERR_PERMISSION`: the provider does not serve this hash now. Excluded
    /// for the hash, no strike.
    Refused { provider: EndpointId, hash: Hash },
    /// `ERR_LIMIT`: the provider is at its upload stream limit. Skipped for
    /// [`LIMIT_RETRY`], no strike.
    Busy { provider: EndpointId },
    /// The provider's bytes failed BLAKE3 verification. Excluded for the
    /// hash (and struck, like any failure).
    Corrupt { provider: EndpointId, hash: Hash },
    /// The provider could not be dialled.
    DialFailed { provider: EndpointId, error: String },
}

type VerdictSink = mpsc::UnboundedSender<LiveVerdict>;

/// Knobs for one [`run_live`].
pub(crate) struct LiveRunOptions {
    /// The progress deadline ([`STALL_HARD_LIMIT`] in production).
    pub stall_hard_limit: Duration,
    /// Race a hedge for the back half of a slow item.
    pub hedging: bool,
    pub telemetry: ProviderTelemetrySink,
    /// Items in flight at once (the collab receive stream limit, L11);
    /// read before each new item is taken, so a change applies live.
    pub max_in_flight: Arc<AtomicUsize>,
    /// Fires when `max_in_flight` changed (Task 15, T12 carry): a raised
    /// limit takes queued items at once instead of at the run's next wake.
    /// `None`: the limit is read at each wake only.
    pub limit_changed: Option<watch::Receiver<usize>>,
    /// The work-unit cap: at a yield, an item larger than this is cut once it
    /// has moved this many bytes since the yield.
    pub unit_cap_bytes: u64,
    /// Fed the exact bytes a transfer just moved, per (item key, provider) —
    /// forwarded into every item's [`AssignmentOptions`] this run builds.
    /// `None` when nothing downstream wants the receive feed.
    pub delivered: Option<DeliveredSink>,
}

/// Why one assignment ended without the item's bytes.
///
/// Every variant carries the bytes the attempt did move, because that is what
/// the provider is credited with and what tells a hedge whether the loser was
/// worth anything.
///
/// `Refused`, `Busy` and `Corrupt` read the provider's answer (spec §7.2,
/// T12 ruling R2).
#[derive(Debug)]
enum TransferFault {
    /// No growth in `bytes_read` for `stall_hard_limit`. No error was raised;
    /// this is a judgement the transport cannot make.
    Stalled { bytes: u64 },
    /// The transfer errored.
    Failed { bytes: u64, error: anyhow::Error },
    /// The dial failed. Struck exactly like `Failed`; told apart only so the
    /// live run can report it ([`LiveVerdict::DialFailed`]).
    DialFailed { error: anyhow::Error },
    /// `ERR_PERMISSION`: this provider does not serve this hash now.
    Refused { bytes: u64 },
    /// `ERR_LIMIT`: this provider is at its upload stream limit.
    Busy { bytes: u64 },
    /// The bytes failed BLAKE3 verification.
    Corrupt { bytes: u64, error: anyhow::Error },
}

impl TransferFault {
    fn bytes(&self) -> u64 {
        match self {
            Self::Stalled { bytes }
            | Self::Failed { bytes, .. }
            | Self::Refused { bytes }
            | Self::Busy { bytes }
            | Self::Corrupt { bytes, .. } => *bytes,
            Self::DialFailed { .. } => 0,
        }
    }

    fn is_stall(&self) -> bool {
        matches!(self, Self::Stalled { .. })
    }

    /// The error a failure carries, for `last_error`.
    fn error(&self) -> Option<&anyhow::Error> {
        match self {
            Self::Failed { error, .. }
            | Self::DialFailed { error }
            | Self::Corrupt { error, .. } => Some(error),
            Self::Stalled { .. } | Self::Refused { .. } | Self::Busy { .. } => None,
        }
    }

    /// One line for a log's `error` field.
    ///
    /// A stall carries no error to render — it is this loop's OWN judgement,
    /// not something the transport reported — so it says so rather than
    /// logging an empty cause, and a real failure renders the anyhow chain the
    /// same way [`record_failure`] stores it.
    fn cause(&self) -> String {
        match self {
            Self::Stalled { .. } => "stalled: no progress within the ceiling".to_string(),
            Self::Failed { error, .. }
            | Self::DialFailed { error }
            | Self::Corrupt { error, .. } => {
                format!("{error:#}")
            }
            Self::Refused { .. } => "refused: the provider does not serve this hash".to_string(),
            Self::Busy { .. } => "busy: the provider is at its upload stream limit".to_string(),
        }
    }
}

/// Read a failed get's refusal code (spec §7.2): `ERR_PERMISSION` is
/// [`TransferFault::Refused`], `ERR_LIMIT` is [`TransferFault::Busy`], a
/// BLAKE3 mismatch is [`TransferFault::Corrupt`], anything else `Failed`.
fn classify_get_error(
    e: &iroh_blobs::get::GetError,
    bytes: u64,
    provider: EndpointId,
) -> TransferFault {
    use iroh_blobs::get::fsm::DecodeError;
    use iroh_blobs::get::GetError;
    match e.iroh_error_code() {
        Some(c) if c == iroh_blobs::protocol::ERR_PERMISSION => {
            return TransferFault::Refused { bytes };
        }
        Some(c) if c == iroh_blobs::protocol::ERR_LIMIT => return TransferFault::Busy { bytes },
        _ => {}
    }
    if let GetError::Decode { source, .. } = e {
        if matches!(
            source,
            DecodeError::ParentHashMismatch { .. } | DecodeError::LeafHashMismatch { .. }
        ) {
            return TransferFault::Corrupt {
                bytes,
                error: anyhow::anyhow!("verification failed from {}: {e}", provider.fmt_short()),
            };
        }
    }
    TransferFault::Failed {
        bytes,
        error: anyhow::anyhow!("get from {}: {e}", provider.fmt_short()),
    }
}

/// One provider's live state, shared by every child in the run.
///
/// `failures` is the CONSECUTIVE-failure counter that drives eviction; it is
/// reset by a success. `stats.failures` is the cumulative report figure and is
/// never reset.
#[derive(Debug, Default)]
struct ProviderState {
    failures: u32,
    next_try: Option<Instant>,
    /// Set by an `ERR_LIMIT` refusal ([`LIMIT_RETRY`]): not a failure, so it
    /// is kept apart from `next_try` and never escalates the ladder.
    busy_until: Option<Instant>,
    inflight: u32,
    /// The most recent `TransferFault::Failed` cause, and when it was recorded
    /// — so a run that exhausts its ladder can say WHY rather than only that it
    /// did. A stall records no cause (its cause is the deadline itself).
    last_error: Option<String>,
    last_error_at: Option<Instant>,
    /// Stalls this provider caused — a strict subset of `stats.failures`, kept
    /// beside the reported stats because [`ProviderStats`]' shape is the
    /// brief's and a stall is a swarm-wide figure, not a per-provider one.
    stalls: u32,
    /// Smoothed goodput in bytes/sec over this provider's COMPLETED transfers,
    /// and how many samples it has. The hedge trigger's `expected` divides by
    /// it; a provider with no completed transfer has none, and borrows the
    /// median of the providers that do.
    goodput: Option<f64>,
    goodput_samples: u32,
    stats: ProviderStats,
}

impl ProviderState {
    /// Neither in backoff nor busy at `now`.
    fn available(&self, now: Instant) -> bool {
        !self.next_try.is_some_and(|t| t > now) && !self.busy_until.is_some_and(|t| t > now)
    }

    /// When it becomes [`available`](Self::available) again, if it is parked
    /// at all.
    fn available_at(&self) -> Option<Instant> {
        match (self.next_try, self.busy_until) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
}

type States = Arc<Mutex<HashMap<EndpointId, ProviderState>>>;

/// A claimed assignment slot on one provider, released on drop.
///
/// The claim has to be structural rather than a paired call: a child task can be
/// ABORTED mid-transfer (the first error in the run drops the `JoinSet`), and a
/// hand-written `release_inflight` after the await would simply never run, so
/// the provider would look permanently busier than it is to every later pick.
struct InflightGuard {
    states: States,
    provider: EndpointId,
}

impl InflightGuard {
    fn provider(&self) -> EndpointId {
        self.provider
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut guard = self
            .states
            .lock()
            .expect("assignment states mutex poisoned");
        if let Some(st) = guard.get_mut(&self.provider) {
            st.inflight = st.inflight.saturating_sub(1);
        }
    }
}

/// What the result loop keeps of an item once the item itself has moved into
/// its task.
struct ItemLabel {
    key: String,
    bytes: u64,
    providers: ProviderSet,
}

/// Log one live item's failure and hand the error back for its outcome —
/// carrying the last provider fault among the ITEM's own providers.
fn isolated_failure(states: &States, label: &ItemLabel, e: anyhow::Error) -> anyhow::Error {
    // A live item every provider refused keeps that cause outermost, so the
    // scheduler reads it as such (its message starts with it).
    let cause = if is_refused_by_every_provider(&e) {
        None
    } else {
        last_failure_cause(states, &label.providers.current())
    };
    let e = match cause {
        Some(c) => e.context(format!("last provider fault: {c}")),
        None => e,
    };
    tracing::warn!(frame_uuid = %label.key, error = %format!("{e:#}"), "blob fetch failed");
    tracing::debug!(
        frame_uuid = %label.key,
        bytes = label.bytes,
        outcome = "failed",
        "blob fetch finished"
    );
    e
}

/// The most recently recorded `TransferFault::Failed` cause among the
/// providers `among`.
fn last_failure_cause(states: &States, among: &[EndpointId]) -> Option<String> {
    let guard = states.lock().expect("assignment states mutex poisoned");
    among
        .iter()
        .filter_map(|p| guard.get(p))
        .filter_map(|st| Some((st.last_error_at?, st.last_error.clone()?)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, cause)| cause)
}

/// The live assignment run (spec §7.2, §8 receive side): the item engine fed
/// from a channel.
///
/// - **Items** arrive on `items`, each with a [`ProviderSet`] (usually live)
///   and its own cancel switch. One is taken only while fewer than
///   `opts.max_in_flight` are in flight and no yield is requested.
/// - **Results** go out on `done` the moment each item ends, so the caller
///   lands a frame at once. Items are isolated: one item's failure is its
///   own.
/// - **Verdicts** — refusals, busy providers, corrupt bytes, failed dials —
///   go out on `verdicts` for the scheduler's per-provider health.
/// - **Cancel.** An item's switch set to `true` stops that item alone; its
///   `GetProgress` stream is dropped, which resets its QUIC stream (module
///   doc), and its verified ranges stay in the store for a resume.
/// - **Yield** (`yield_now` = `true`, a personal transfer is waiting — §8):
///   no new item is taken; an in-flight item that is not transferring (it
///   waits for a provider) is cut at once ([`ItemOutcome::Cancelled`]), and
///   a transferring one larger than `opts.unit_cap_bytes` once it has moved
///   that much since the yield; the run returns as soon as nothing is in
///   flight.
///
/// Returns when `items` is closed and drained, or at a yield once nothing is
/// in flight. Per-provider state (backoff, busy, goodput) lives for the run.
///
/// Items still queued in the channel when a yield returns are dropped with
/// the receiver and get NO outcome on `done`: the caller re-derives them
/// from its need set (T15).
pub(crate) async fn run_live(
    store: &Store,
    dialer: Dialer,
    mut items: mpsc::Receiver<LiveItem>,
    opts: LiveRunOptions,
    done: mpsc::UnboundedSender<(String, ItemOutcome)>,
    verdicts: mpsc::UnboundedSender<LiveVerdict>,
    mut yield_now: watch::Receiver<bool>,
) -> AssignmentReport {
    let remote: Remote = store.remote().clone();
    let blobs: Blobs = store.blobs().clone();
    // The hedge budget follows the items in flight (`HedgeBudget::extend` /
    // `retire`), so there is no fixed total to take 5 % of.
    let ledger: Ledger = Arc::new(Mutex::new(HedgeLedger {
        budget: HedgeBudget::live(),
        completions: CompletionWindow::default(),
        hedges: 0,
        hedge_bytes: 0,
    }));
    let states: States = Arc::new(Mutex::new(HashMap::new()));
    let child_opts = AssignmentOptions {
        stall_hard_limit: opts.stall_hard_limit,
        hedging: opts.hedging,
        telemetry: Arc::clone(&opts.telemetry),
        delivered: opts.delivered.clone(),
    };
    let mut set: tokio::task::JoinSet<ItemOutcome> = tokio::task::JoinSet::new();
    let mut labels: HashMap<tokio::task::Id, ItemLabel> = HashMap::new();
    let mut items_open = true;
    let mut yield_open = true;
    let mut taken = 0usize;
    let mut limit_changed = opts.limit_changed.clone();

    loop {
        let yielding = *yield_now.borrow_and_update();
        if set.is_empty() && (yielding || !items_open) {
            break;
        }
        let cap = opts.max_in_flight.load(Ordering::Relaxed).max(1);
        let can_take = items_open && !yielding && set.len() < cap;
        tokio::select! {
            joined = set.join_next_with_id(), if !set.is_empty() => {
                let (id, outcome) = match joined {
                    Some(Ok((id, outcome))) => (id, outcome),
                    Some(Err(join)) => {
                        let id = join.id();
                        let e = anyhow::Error::new(join).context("assignment task panicked");
                        tracing::error!(error = %format!("{e:#}"), "live assignment task panicked");
                        (id, ItemOutcome::Failed(e))
                    }
                    // Guarded by `!set.is_empty()`.
                    None => continue,
                };
                let Some(label) = labels.remove(&id) else {
                    debug_assert!(false, "every live task is registered under its id");
                    tracing::error!("live assignment task finished unregistered");
                    continue;
                };
                ledger
                    .lock()
                    .expect("hedge ledger mutex poisoned")
                    .budget
                    .retire(label.bytes);
                let outcome = match outcome {
                    ItemOutcome::Failed(e) => ItemOutcome::Failed(isolated_failure(&states, &label, e)),
                    other => {
                        tracing::debug!(
                            frame_uuid = %label.key,
                            bytes = label.bytes,
                            outcome = if matches!(other, ItemOutcome::Done) { "ok" } else { "cancelled" },
                            "blob fetch finished"
                        );
                        other
                    }
                };
                send_done(&done, label.key, outcome);
            }
            next = items.recv(), if can_take => match next {
                Some(live) => {
                    taken += 1;
                    let LiveItem { item, cancel } = live;
                    if *cancel.borrow() {
                        tracing::debug!(
                            frame_uuid = %item.key,
                            bytes = item.size,
                            outcome = "cancelled",
                            "blob fetch finished"
                        );
                        send_done(&done, item.key, ItemOutcome::Cancelled);
                        continue;
                    }
                    ledger
                        .lock()
                        .expect("hedge ledger mutex poisoned")
                        .budget
                        .extend(item.size);
                    let label = ItemLabel {
                        key: item.key.clone(),
                        bytes: item.size,
                        providers: item.providers.clone(),
                    };
                    let (hash, size) = (item.hash, item.size);
                    let transferring = Arc::new(AtomicBool::new(false));
                    let child = run_child(
                        dialer.clone(),
                        remote.clone(),
                        blobs.clone(),
                        Arc::clone(&states),
                        Arc::clone(&ledger),
                        item,
                        child_opts.clone(),
                        verdicts.clone(),
                        Arc::clone(&transferring),
                    );
                    let cut = cut_at_yield(
                        yield_now.clone(),
                        blobs.clone(),
                        hash,
                        size,
                        opts.unit_cap_bytes,
                        transferring,
                    );
                    let handle = set.spawn(async move {
                        tokio::select! {
                            r = child => match r {
                                Ok(()) => ItemOutcome::Done,
                                Err(e) => ItemOutcome::Failed(e),
                            },
                            _ = cancelled(cancel) => ItemOutcome::Cancelled,
                            _ = cut => ItemOutcome::Cancelled,
                        }
                    });
                    labels.insert(handle.id(), label);
                }
                None => items_open = false,
            },
            changed = yield_now.changed(), if yield_open => {
                if changed.is_err() {
                    // The sender is gone: its last value stands.
                    yield_open = false;
                }
            }
            changed = async {
                match limit_changed.as_mut() {
                    Some(rx) => rx.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_err() {
                    // The sender is gone: the limit is read at each wake.
                    limit_changed = None;
                }
            }
        }
    }

    let report = report_from_with_ledger(&states, &ledger);
    tracing::debug!(
        count = taken,
        bytes = report.total_bytes(),
        "live assignment run finished"
    );
    report
}

/// Hand one live item's outcome to the caller.
fn send_done(
    done: &mpsc::UnboundedSender<(String, ItemOutcome)>,
    key: String,
    outcome: ItemOutcome,
) {
    if let Err(mpsc::error::SendError((key, outcome))) = done.send((key, outcome)) {
        tracing::warn!(
            frame_uuid = %key,
            outcome = ?outcome,
            "live fetch result dropped: nobody is listening"
        );
    }
}

/// Resolves once the item's cancel switch reads `true`. A switch whose
/// sender is gone without having been set never fires.
async fn cancelled(mut cancel: watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() {
            return;
        }
        if cancel.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Resolves when a yield should cut this item (§8 — a personal transfer
/// waits at most one frame):
///
/// - an item that is NOT transferring (waiting for a provider to appear, on
///   parked or busy providers) is cut at once — it would otherwise hold its
///   stream slot and keep the yield from ever completing;
/// - a transferring item larger than the work-unit cap is cut once it has
///   moved `cap` bytes since the yield began; a smaller one finishes.
///
/// A withdrawn yield starts the count again at the next one.
async fn cut_at_yield(
    mut yield_now: watch::Receiver<bool>,
    blobs: Blobs,
    hash: Hash,
    size: u64,
    cap: u64,
    transferring: Arc<AtomicBool>,
) {
    loop {
        // Wait for a yield.
        while !*yield_now.borrow_and_update() {
            if yield_now.changed().await.is_err() {
                return std::future::pending().await;
            }
        }
        let base = if size > cap {
            Some(local_bytes(&blobs, hash).await)
        } else {
            None
        };
        while *yield_now.borrow() {
            if !transferring.load(Ordering::Acquire) {
                tracing::debug!(
                    blake3 = %hash,
                    "live fetch cut at a yield: not transferring"
                );
                return;
            }
            tokio::time::sleep(YIELD_CUT_POLL).await;
            if let Some(base) = base {
                if local_bytes(&blobs, hash).await.saturating_sub(base) >= cap {
                    tracing::debug!(
                        blake3 = %hash,
                        bytes = cap,
                        "live fetch cut at a yield: the work-unit cap moved"
                    );
                    return;
                }
            }
        }
    }
}

/// The verified bytes of `hash` in the store now (0 when it cannot say).
async fn local_bytes(blobs: &Blobs, hash: Hash) -> u64 {
    match blobs.observe(hash).await {
        Ok(bitfield) => bitfield.total_bytes(),
        Err(e) => {
            tracing::debug!(blake3 = %hash, error = %e, "observe for the yield cut failed");
            0
        }
    }
}

/// One child: pick a provider, transfer what is still missing, repeat until the
/// child is complete locally or the ladder runs out.
///
/// `verdicts` is the live run's channel ([`run_live`]); `transferring` is the
/// item's flag for [`cut_at_yield`].
#[allow(clippy::too_many_arguments)]
async fn run_child(
    dialer: Dialer,
    remote: Remote,
    blobs: Blobs,
    states: States,
    ledger: Ledger,
    item: FetchItem,
    opts: AssignmentOptions,
    verdicts: VerdictSink,
    transferring: Arc<AtomicBool>,
) -> Result<()> {
    let request = item.request.clone();
    // Log field names are a spec: `root_hash` is the hash the request
    // addresses (the raw blob itself), `child` is the item's key.
    let root = item.request.hash;
    let index = item.key.as_str();
    let hash = item.hash;
    let mut providers = item.providers.clone();
    // Providers this item will not ask again: one that refused it
    // (`ERR_PERMISSION`) or whose bytes failed verification (spec §7.2, §7.3).
    let mut excluded: HashSet<EndpointId> = HashSet::new();
    let faults = FaultScope {
        states: &states,
        opts: &opts,
        verdicts: &verdicts,
        key: index,
        hash,
    };
    let mut rounds = 0u32;
    // Set when a hedge wins: the rest of this child goes to the provider that
    // just proved it can move bytes, not back to the one we gave up on.
    let mut forced: Option<EndpointId> = None;
    'child: loop {
        // Recomputed EVERY round, so a reassignment asks the next provider only
        // for the bytes still missing, resuming at the byte.
        let local = remote
            .local_for_request(request.clone())
            .await
            .map_err(|e| anyhow::anyhow!("local info for {}: {e}", item.describe()))?;
        if local.is_complete() {
            return Ok(());
        }
        let missing = local.missing();

        // This round's candidates: the providers now, minus the excluded.
        let current = providers.current_seen();
        if providers.is_live() {
            // A provider that left the live set and came back re-announced
            // itself as serving (spec §4.2): its exclusion for this item ends
            // the moment it is seen absent.
            excluded.retain(|p| current.contains(p));
        }
        let candidates: Vec<EndpointId> = current
            .iter()
            .copied()
            .filter(|p| !excluded.contains(p))
            .collect();
        if candidates.is_empty() {
            if providers.is_live() && !current.is_empty() {
                // Every provider of a live set refused this item: it fails to
                // the scheduler, which owns when to try again — waiting here
                // would hold a stream slot for as long as the refusals last.
                return Err(anyhow::Error::new(RefusedByEveryProvider {
                    item: item.describe(),
                }));
            }
            // Nobody to ask. A live set waits for a provider to appear, for
            // as long as it takes and without spending a round (spec §7.2: a
            // frame without providers sleeps and costs nothing); a fixed list
            // — or a live one whose sender is gone — never grows.
            set_transferring(&transferring, false);
            if providers.changed().await {
                continue;
            }
            if excluded.is_empty() {
                anyhow::bail!("{}: no provider to assign it to", item.describe());
            }
            anyhow::bail!(
                "{}: every provider refused it or served bytes that failed verification",
                item.describe()
            );
        }

        let Some(claim) = claim_provider(&states, &candidates, forced.take()) else {
            // Every candidate is parked. Wait for the earliest of them and
            // try again — bounded, never a spin. Waiting on a provider that is
            // merely BUSY (`ERR_LIMIT`) spends no round: it is alive and will
            // free a stream.
            let busy = waiting_on_busy(&states, &candidates);
            if !busy {
                rounds += 1;
                if rounds > MAX_BACKOFF_ROUNDS {
                    anyhow::bail!(
                        "{}: every provider exhausted after {rounds} rounds",
                        item.describe()
                    );
                }
            }
            let wait = earliest_wait(&states, &candidates);
            if busy {
                tracing::debug!(
                    frame_uuid = index,
                    blake3 = %hash,
                    "every provider busy — waiting for a free stream"
                );
            } else {
                tracing::debug!(
                    root_hash = %root,
                    child = index,
                    attempt = rounds,
                    delay_ms = wait.as_millis() as u64,
                    "every provider in backoff — waiting for the earliest"
                );
            }
            set_transferring(&transferring, false);
            let timer = providers.sleep_or_change(wait).await;
            if !busy && !timer {
                // A live set changed before the backoff ran out: nothing was
                // tried and nothing waited out, so the round is given back.
                rounds -= 1;
            }
            if busy && providers.is_live() && !excluded.is_empty() {
                // A busy park ended (Task 15, T12 minor): a provider this
                // item excluded after a refusal may serve again by now (a
                // rehash cleared its mismatch, its storage came back) —
                // without being seen absent from the live set in between.
                // Ask it again rather than waiting on the busy one alone.
                tracing::debug!(
                    frame_uuid = index,
                    blake3 = %hash,
                    count = excluded.len(),
                    "busy wait ended; refused providers asked again"
                );
                excluded.clear();
            }
            continue;
        };

        // From the claim on, the item is transferring until it next waits
        // (see `set_transferring`); a yield cuts a live item that is not
        // (see `cut_at_yield`).
        set_transferring(&transferring, true);
        let provider = claim.provider();
        (opts.telemetry)(ProviderEvent::Trying(*provider.as_bytes()));
        let started = Instant::now();

        // ── the race ────────────────────────────────────────────────────────
        // The primary keeps the whole missing range. A hedge, once the trigger
        // and the budget allow one, takes the BACK half of what is still
        // missing from a different provider; whichever side finishes, the other
        // is dropped, which resets its QUIC stream (see the module doc).
        let primary_progress = Arc::new(AtomicU64::new(0));
        let on_bytes = opts.delivered.as_ref().map(|sink| {
            let sink = Arc::clone(sink);
            let key = item.key.clone();
            let who = *provider.as_bytes();
            Arc::new(move |d: u64| sink(&key, who, d)) as Arc<dyn Fn(u64) + Send + Sync>
        });
        let mut primary: BoxedTransfer = Box::pin(transfer_once(
            dialer.clone(),
            remote.clone(),
            provider,
            missing,
            opts.stall_hard_limit,
            Arc::clone(&primary_progress),
            on_bytes,
        ));
        let mut hedge: Option<HedgeRun> = None;
        // Set when the primary failed while a hedge was live: the hedge is
        // promoted and carries the round alone (ruling R2). `primary` is then a
        // future that never completes, so the race is the hedge and nothing
        // else, and no second hedge is armed on a provider that is already out.
        let mut primary_gone = false;
        let outcome = loop {
            let armed = hedge.is_some();
            let step = tokio::select! {
                r = &mut primary => Step::Primary(r),
                r = poll_slot(hedge.as_mut().map(|h| &mut h.fut)) => Step::Hedge(r),
                _ = tokio::time::sleep(HEDGE_REEVALUATE_INTERVAL),
                    if !armed && opts.hedging && !primary_gone => Step::Reevaluate,
            };
            match step {
                Step::Primary(Ok(stats)) => {
                    if let Some(h) = hedge.take() {
                        // The primary finished the WHOLE missing range, so every
                        // byte the hedge moved was fetched twice. Its own
                        // progress is already request-relative to the hedged
                        // range, so it needs no offsetting.
                        let duplicated = h.progress.load(Ordering::Relaxed).min(h.charge);
                        settle_hedge_loser(&ledger, &h, duplicated);
                        tracing::debug!(
                            child = index,
                            loser = %h.provider.fmt_short(),
                            refunded_bytes = h.charge - duplicated,
                            "swarm hedge loser cancelled"
                        );
                    }
                    break Ok(stats);
                }
                Step::Primary(Err(fault)) => {
                    let Some(_) = hedge.as_ref() else {
                        break Err(fault);
                    };
                    // Ruling R2: a hedge is a real transfer, not a speculative
                    // copy to be thrown away because its primary died. Record
                    // the primary's failure, then let the hedge finish and
                    // become this round's result; the remainder is picked up by
                    // the next round, on the hedge's provider.
                    note_fault(&faults, &mut excluded, provider, &fault, started.elapsed());
                    tracing::debug!(
                        root_hash = %root,
                        child = index,
                        provider = %provider.fmt_short(),
                        bytes = fault.bytes(),
                        error = %fault.cause(),
                        "primary failed while a hedge was live — promoting the hedge"
                    );
                    // The `let Some(_) = hedge.as_ref() else { break }` above
                    // is exactly this invariant; restated here so a future
                    // reorder of this arm trips in debug builds instead of
                    // silently promoting nothing.
                    debug_assert!(hedge.is_some(), "primary_gone set with no hedge live");
                    primary_gone = true;
                    primary = Box::pin(std::future::pending());
                }
                Step::Hedge(r) => {
                    let h = hedge.take().expect("the hedge branch only runs when armed");
                    match r {
                        Ok(stats) => {
                            // Sampled before the completeness query below, so
                            // the local store's latency is not billed to the
                            // peer as transfer time.
                            let hedge_elapsed = h.started.elapsed();
                            // Ruling R3: a child is credited to whichever
                            // transfer made it complete. A hedge usually
                            // finishes only its own half and the round below
                            // that fetches the remainder does the crediting —
                            // but when the primary had already delivered
                            // everything in FRONT of the cut, the hedge's back
                            // half is the last of the child, and the
                            // `continue 'child` below then returns through the
                            // top-of-loop `is_complete` early return, which
                            // records nothing at all. Without asking here, such
                            // a child would be counted for NOBODY and
                            // `total_children` would quietly under-report. An
                            // error is read as "not complete": the next round's
                            // own `local_for_request` hits the same fault and
                            // propagates it, so nothing is swallowed.
                            let completed_child = remote
                                .local_for_request(request.clone())
                                .await
                                .map(|l| l.is_complete())
                                .unwrap_or(false);
                            record_success(
                                &states,
                                h.provider,
                                &stats,
                                hedge_elapsed,
                                completed_child,
                            );
                            if completed_child {
                                // The hedge finished the child outright (ruling
                                // R3's "the last of the child" case above) — it
                                // is a completed assignment exactly like the
                                // unhedged success path below, so it feeds the
                                // same p95 window and earns the same budget
                                // refill.
                                note_completion(&ledger, hedge_elapsed, stats.payload_bytes_read);
                            }
                            let moved = primary_progress.load(Ordering::Relaxed);
                            // `moved` counts payload bytes inside the PRIMARY's
                            // request, which began at ROUND START; the hedged
                            // half begins `armed_at + split_rel` into that same
                            // request (see `HedgeRun::armed_at`). What the
                            // primary moved PAST that point, and only that, was
                            // fetched twice.
                            let duplicated = moved
                                .saturating_sub(h.armed_at.saturating_add(h.split_rel))
                                .min(h.charge);
                            settle_hedge_loser(&ledger, &h, duplicated);
                            if !primary_gone {
                                // Cancelling the primary IS dropping it; the
                                // `continue` below ends its scope.
                                drop(primary);
                                tracing::debug!(
                                    child = index,
                                    loser = %provider.fmt_short(),
                                    refunded_bytes = h.charge - duplicated,
                                    "swarm hedge loser cancelled"
                                );
                            }
                            // D4 §4.5: the rest of the child goes to the hedge's
                            // provider, not back to the one we just gave up on.
                            forced = Some(h.provider);
                            continue 'child;
                        }
                        Err(fault) => {
                            // A failed hedge is the hedge's provider's problem,
                            // never the child's — unless it was carrying the
                            // round alone, in which case it IS the round's
                            // outcome and the loop below reassigns.
                            note_fault(
                                &faults,
                                &mut excluded,
                                h.provider,
                                &fault,
                                h.started.elapsed(),
                            );
                            settle_hedge_loser(&ledger, &h, fault.bytes().min(h.charge));
                            if primary_gone {
                                // Nothing is left running. Both failures are
                                // already recorded against their OWN providers,
                                // so this round is over — and it must not
                                // `break` with the hedge's fault, which the
                                // round's epilogue would bill to the primary's
                                // provider a second time.
                                continue 'child;
                            }
                        }
                    }
                }
                Step::Reevaluate => {
                    hedge = try_arm_hedge(
                        &dialer,
                        &remote,
                        &blobs,
                        &states,
                        &ledger,
                        &opts,
                        &item,
                        &candidates,
                        provider,
                        started,
                        &primary_progress,
                    )
                    .await;
                }
            }
        };
        let elapsed = started.elapsed();
        drop(claim);

        match outcome {
            // A completed `execute_get` means the requested range is decoded,
            // BLAKE3-verified and imported, so the child is complete — the same
            // conclusion upstream's `execute_get` draws from the same `Ok`. We
            // deliberately do not spend another `local_for_request` to re-prove
            // it: the caller exports every child right after this returns, so an
            // incomplete one fails loudly there rather than passing silently.
            Ok(stats) => {
                record_success(&states, provider, &stats, elapsed, true);
                note_completion(&ledger, elapsed, stats.payload_bytes_read);
                tracing::debug!(
                    root_hash = %root,
                    child = index,
                    child_hash = %hash,
                    provider = %provider.fmt_short(),
                    bytes = stats.payload_bytes_read,
                    duration_ms = elapsed.as_millis() as u64,
                    "child assigned and delivered"
                );
                return Ok(());
            }
            Err(fault) => {
                note_fault(&faults, &mut excluded, provider, &fault, elapsed);
                match &fault {
                    TransferFault::Stalled { bytes } => tracing::warn!(
                        root_hash = %root,
                        child = index,
                        child_hash = %hash,
                        provider = %provider.fmt_short(),
                        bytes = *bytes,
                        timeout_ms = opts.stall_hard_limit.as_millis() as u64,
                        "provider stalled on this child — reassigning"
                    ),
                    TransferFault::Failed { bytes, error } => tracing::debug!(
                        root_hash = %root,
                        child = index,
                        child_hash = %hash,
                        provider = %provider.fmt_short(),
                        bytes = *bytes,
                        error = %format!("{error:#}"),
                        "provider failed on this child — reassigning"
                    ),
                    TransferFault::DialFailed { error } => tracing::debug!(
                        root_hash = %root,
                        child = index,
                        child_hash = %hash,
                        provider = %provider.fmt_short(),
                        bytes = 0u64,
                        error = %format!("{error:#}"),
                        "provider failed on this child — reassigning"
                    ),
                    // Logged by `note_fault`, which read the provider's answer.
                    TransferFault::Refused { .. }
                    | TransferFault::Busy { .. }
                    | TransferFault::Corrupt { .. } => {}
                }
                // Round again: `local_for_request` above recomputes the missing
                // range, so the next provider resumes at the byte.
            }
        }
    }
}

/// What [`note_fault`] needs from its child.
struct FaultScope<'a> {
    states: &'a States,
    opts: &'a AssignmentOptions,
    verdicts: &'a VerdictSink,
    /// The item's key (a frame uuid).
    key: &'a str,
    hash: Hash,
}

impl FaultScope<'_> {
    fn verdict(&self, v: LiveVerdict) {
        if self.verdicts.send(v).is_err() {
            tracing::debug!(
                frame_uuid = self.key,
                "live verdict dropped: nobody is listening"
            );
        }
    }
}

/// Book one provider's fault against it (spec §7.2, §7.3, T12 ruling R2).
///
/// - `Stalled`, `Failed`, `DialFailed`: the backoff ladder
///   ([`record_failure`]) and a `Failed` telemetry event.
/// - `Busy` (`ERR_LIMIT`): the provider is skipped for [`LIMIT_RETRY`] for
///   every hash; no strike, no fault telemetry.
/// - `Refused` (`ERR_PERMISSION`): excluded for this item only; no strike,
///   no fault telemetry. On a live set the exclusion ends when the provider
///   leaves the set and comes back.
/// - `Corrupt` (verification failed): struck like any failure and excluded
///   for this item.
fn note_fault(
    scope: &FaultScope<'_>,
    excluded: &mut HashSet<EndpointId>,
    provider: EndpointId,
    fault: &TransferFault,
    elapsed: Duration,
) {
    match fault {
        TransferFault::Busy { bytes } => {
            mark_busy(
                scope.states,
                provider,
                *bytes,
                elapsed,
                Instant::now() + LIMIT_RETRY,
            );
            tracing::debug!(
                provider = %provider.fmt_short(),
                frame_uuid = scope.key,
                blake3 = %scope.hash,
                "provider at its upload stream limit — trying another"
            );
            scope.verdict(LiveVerdict::Busy { provider });
        }
        TransferFault::Refused { bytes } => {
            // Not a provider fault: no strike and no `Failed` telemetry.
            excluded.insert(provider);
            credit_attempt(scope.states, provider, *bytes, elapsed);
            tracing::debug!(
                provider = %provider.fmt_short(),
                frame_uuid = scope.key,
                blake3 = %scope.hash,
                "provider refused this frame — excluded for it"
            );
            scope.verdict(LiveVerdict::Refused {
                provider,
                hash: scope.hash,
            });
        }
        TransferFault::Corrupt { error, .. } => {
            record_failure(scope.states, provider, fault, elapsed);
            (scope.opts.telemetry)(ProviderEvent::Failed(*provider.as_bytes()));
            tracing::warn!(
                provider = %provider.fmt_short(),
                frame_uuid = scope.key,
                blake3 = %scope.hash,
                error = %format!("{error:#}"),
                "provider served bytes that failed verification"
            );
            excluded.insert(provider);
            scope.verdict(LiveVerdict::Corrupt {
                provider,
                hash: scope.hash,
            });
        }
        TransferFault::DialFailed { error } => {
            record_failure(scope.states, provider, fault, elapsed);
            (scope.opts.telemetry)(ProviderEvent::Failed(*provider.as_bytes()));
            scope.verdict(LiveVerdict::DialFailed {
                provider,
                error: format!("{error:#}"),
            });
        }
        TransferFault::Stalled { .. } | TransferFault::Failed { .. } => {
            record_failure(scope.states, provider, fault, elapsed);
            (scope.opts.telemetry)(ProviderEvent::Failed(*provider.as_bytes()));
        }
    }
}

/// A BLAKE3 chunk, the unit `ChunkRanges` counts in.
const CHUNK_BYTES: u64 = 1024;

/// Where a missing range was cut in two, and what each side is worth.
struct MissingSplit {
    /// The back half, as ranges — what the hedge asks for.
    back: ChunkRanges,
    /// Bytes inside `back`. This is the charge, and it is the byte extent of
    /// the RANGES, never `size - split`: a cancelled hedge leaves the missing
    /// set as `[0,x) ∪ [y,size)`, and measuring the charge against the blob's
    /// end would bill the budget for bytes that are already present.
    charge: u64,
    /// Bytes of the missing set that come BEFORE the cut — relative to the
    /// missing set AS OBSERVED AT ARM TIME, not to the primary's own request
    /// (which began at ROUND START, before this hedge existed). That is
    /// exactly why [`HedgeRun::armed_at`] exists: turning the primary's
    /// request-relative progress into an arm-time-relative one needs both
    /// numbers together, `split_rel` alone is not enough.
    split_rel: u64,
}

/// Bytes of the chunk span `[start, end)` that fall inside a blob of `size`.
fn span_bytes(start: u64, end: u64, size: u64) -> u64 {
    let s = start.saturating_mul(CHUNK_BYTES).min(size);
    let e = end.saturating_mul(CHUNK_BYTES).min(size);
    e.saturating_sub(s)
}

/// Cut a missing set in half BY CHUNK COUNT, wherever its spans happen to lie.
///
/// `ChunkRanges` is a boundary list — `[b0,b1) ∪ [b2,b3) ∪ …`, with an open
/// tail when the count is odd — so the spans are walked directly rather than
/// assumed to be one contiguous prefix-complement. Returns `None` when there
/// is nothing worth splitting: an empty set, or a remainder so small that one
/// side of the cut would be empty (a single chunk left is not worth racing).
fn split_missing_at_midpoint(missing: &ChunkRanges, size: u64) -> Option<MissingSplit> {
    let last_chunk = size.div_ceil(CHUNK_BYTES);
    let boundaries = missing.boundaries();
    let mut spans: Vec<(u64, u64)> = Vec::new();
    let mut i = 0;
    while i < boundaries.len() {
        let start = boundaries[i].0;
        let end = boundaries
            .get(i + 1)
            .map(|c| c.0)
            .unwrap_or(last_chunk)
            .min(last_chunk);
        if end > start {
            spans.push((start, end));
        }
        i += 2;
    }
    let total: u64 = spans.iter().map(|(a, b)| span_bytes(*a, *b, size)).sum();
    if total == 0 {
        return None;
    }

    // Walk to the halfway byte and cut at the chunk boundary at or past it.
    let target = total / 2;
    let mut front_bytes = 0u64;
    let mut split_chunk = spans[0].0;
    for (a, b) in &spans {
        let bytes = span_bytes(*a, *b, size);
        if front_bytes + bytes >= target {
            let need = target - front_bytes;
            let chunks_in = need.div_ceil(CHUNK_BYTES).min(b - a);
            split_chunk = a + chunks_in;
            front_bytes += span_bytes(*a, split_chunk, size);
            break;
        }
        front_bytes += bytes;
        split_chunk = *b;
    }

    let back = missing.clone() & ChunkRanges::chunks(split_chunk..);
    let charge = total - front_bytes;
    if charge == 0 || front_bytes == 0 || back.is_empty() {
        return None;
    }
    Some(MissingSplit {
        back,
        charge,
        split_rel: front_bytes,
    })
}

/// The hedge's request: `request` narrowed to the `back` ranges of the blob it
/// lands.
///
/// A raw blob request (`GetRequest::blob`) becomes
/// `GetRequest::blob_ranges(hash, back)`; a request for one child of a
/// collection (`child(i).build(root)`, i.e. one non-empty range at offset
/// `i + 1`) is narrowed to `child(i, back).build(root)`. Any other shape gets
/// no hedge (`None`) rather than a guessed request.
pub(crate) fn hedge_back_half_request(
    request: &GetRequest,
    hash: Hash,
    back: ChunkRanges,
) -> Option<GetRequest> {
    if request.ranges.is_blob() {
        return Some(GetRequest::blob_ranges(hash, back));
    }
    let (offset, _) = request.ranges.as_single()?;
    let index = offset.checked_sub(1)?;
    Some(GetRequest::builder().child(index, back).build(request.hash))
}

/// A hedge assignment in flight beside its primary.
struct HedgeRun {
    provider: EndpointId,
    fut: BoxedTransfer,
    progress: Arc<AtomicU64>,
    /// Bytes charged to the budget when this hedge was armed — the size of the
    /// back range it asked for.
    charge: u64,
    /// How many bytes of the primary's OWN request come before the hedged
    /// range, so its progress counter can be turned into "how much of the
    /// charged range did it duplicate". Request-relative, never a blob offset
    /// — see [`MissingSplit::split_rel`].
    ///
    /// Relative to the missing set AS IT STOOD WHEN THIS HEDGE WAS ARMED, which
    /// is not where the primary's request began — hence `armed_at` below.
    split_rel: u64,
    /// The primary's progress at the instant the split was cut.
    ///
    /// `split_rel` indexes the missing set observed at arm time, while the
    /// primary's counter indexes the request it was handed at ROUND START; by
    /// arm time the primary has already consumed `armed_at` bytes of that
    /// request. The hedged range therefore begins at `armed_at + split_rel` in
    /// the primary's frame, and comparing its progress against `split_rel`
    /// alone over-counts duplication by everything the primary moved before the
    /// hedge existed — on a 1 MiB child with the primary at 600 KiB when the
    /// hedge took the last 212 KiB, that read as 488 KiB duplicated (clamped to
    /// the whole charge, refunding nothing) where the truth was zero.
    armed_at: u64,
    started: Instant,
    /// Keeps the hedge provider's assignment slot claimed for as long as the
    /// hedge runs; released structurally when this struct drops.
    _claim: InflightGuard,
}

type BoxedTransfer =
    std::pin::Pin<Box<dyn std::future::Future<Output = TransferResult> + Send + 'static>>;
type TransferResult = std::result::Result<Stats, TransferFault>;

/// What one turn of the race produced.
enum Step {
    Primary(TransferResult),
    Hedge(TransferResult),
    Reevaluate,
}

/// Await an optional future, or never.
///
/// Borrowing the slot only for the duration of the `select!` is what lets the
/// handler bodies take the hedge out of it afterwards; dropping THIS future
/// does not drop the boxed transfer it is polling, which is exactly right — a
/// hedge survives every turn of the loop until someone takes it.
async fn poll_slot(slot: Option<&mut BoxedTransfer>) -> TransferResult {
    match slot {
        Some(f) => f.await,
        None => std::future::pending().await,
    }
}

/// A completed assignment: refill the budget and widen the p95 sample.
fn note_completion(ledger: &Ledger, elapsed: Duration, bytes: u64) {
    let mut l = ledger.lock().expect("hedge ledger mutex poisoned");
    l.completions.record(elapsed);
    l.budget.earn(bytes);
}

/// Settle a hedge: give back the part of its charge that never became a
/// duplicate byte.
///
/// `duplicated` is how much of the CHARGED range the loser actually moved —
/// the hedge's own progress when the primary won, or the primary's progress
/// past the split point when the hedge won. A hedge that wins outright against
/// a primary still below the split therefore costs nothing, which is the honest
/// answer: nothing was fetched twice.
fn settle_hedge_loser(ledger: &Ledger, hedge: &HedgeRun, duplicated: u64) {
    let duplicated = duplicated.min(hedge.charge);
    let refund = hedge.charge - duplicated;
    let mut l = ledger.lock().expect("hedge ledger mutex poisoned");
    l.budget.refund(refund);
    // What the bucket did NOT get back is what hedging actually cost.
    l.hedge_bytes = l.hedge_bytes.saturating_add(duplicated);
}

/// Decide whether to hedge this assignment, and start one if so.
///
/// Returns `None` for every "not yet": size not known, nothing missing, no
/// goodput to judge by, still inside the deadline, no second provider, or the
/// budget refusing. The caller simply asks again.
#[allow(clippy::too_many_arguments)]
async fn try_arm_hedge(
    dialer: &Dialer,
    remote: &Remote,
    blobs: &Blobs,
    states: &States,
    ledger: &Ledger,
    opts: &AssignmentOptions,
    item: &FetchItem,
    candidates: &[EndpointId],
    primary: EndpointId,
    started: Instant,
    primary_progress: &Arc<AtomicU64>,
) -> Option<HedgeRun> {
    // The round's candidates: the item's providers minus the ones it excluded.
    let providers: &[EndpointId] = candidates;
    let index = item.key.as_str();
    let hash = item.hash;
    // What is still missing, from the progress truth (`store.observe`) rather
    // than from the primary's stream.
    // The local bitfield carries no size until the store flushes its first
    // batch, i.e. until a whole 16 KiB leaf has arrived — 0.3 s at 50 KB/s,
    // 16 s at 1 KB/s. We do NOT work around that: without a size there is no
    // midpoint to split at, and a peer delivering nothing at all is the STALL
    // CEILING's case, not the hedge's. The two rules divide the space between
    // them — the ceiling handles "not moving", the hedge handles "moving, but
    // far too slowly for this swarm".
    let bitfield = blobs.observe(hash).await.ok()?;
    // Read against the SAME snapshot the split below is cut from: `split_rel`
    // is an offset into this bitfield's missing set, so the primary's progress
    // has to be sampled here, beside it, and not at any later point where the
    // two would describe different instants.
    let armed_at = primary_progress.load(Ordering::Relaxed);
    let size = bitfield.size();
    if size == 0 || bitfield.is_complete() {
        return None;
    }
    let missing_bytes = size.saturating_sub(bitfield.total_bytes());
    if missing_bytes == 0 {
        return None;
    }

    // Is it late? `expected` is what this provider's own measured goodput says
    // the rest should take; the p95 is what recent assignments actually took.
    //
    // Measured consequence, worth knowing before reading a log: because the
    // deadline is the LARGER of the two and `expected` divides by the
    // provider's OWN goodput, a CONSISTENTLY slow peer earns protection from
    // hedging as soon as it completes one transfer — it is never late by its
    // own standard. A 50 KB/s peer with 256 KiB left gets a 10.5 s deadline. An
    // instrumented run of 21 children showed 388 refusals on this branch
    // against 6 hedges armed. That is D4 §4.5 as written, not a defect: hedging
    // is for the TAIL, and a uniformly slow peer is the stall ceiling's and
    // A4's ranking problem, not the hedge's.
    let goodput = ewma_goodput(states, primary)?;
    let expected = Duration::from_secs_f64((missing_bytes as f64 / goodput.max(1.0)).min(86_400.0));
    let p95 = ledger
        .lock()
        .expect("hedge ledger mutex poisoned")
        .completions
        .p95();
    let deadline = p95
        .unwrap_or(Duration::ZERO)
        .max(expected.mul_f64(HEDGE_EXPECTED_MULTIPLIER));
    if started.elapsed() < deadline {
        return None;
    }

    // Somebody else has to be able to take it.
    let others: Vec<EndpointId> = providers
        .iter()
        .copied()
        .filter(|p| *p != primary)
        .collect();
    if others.is_empty() {
        return None;
    }
    let claim = claim_provider(states, &others, None)?;

    // The back half of what is still missing, cut at the missing set's OWN
    // chunk-count midpoint — no assumption that the present bytes form a
    // prefix, because after a cancelled hedge they do not.
    let missing_ranges = ChunkRanges::bytes(0..size) - bitfield.ranges.clone();
    let split = split_missing_at_midpoint(&missing_ranges, size)?;
    let charge = split.charge;
    // Built BEFORE the charge, so a request shape we cannot narrow costs the
    // budget nothing.
    let request = hedge_back_half_request(&item.request, hash, split.back)?;

    if !ledger
        .lock()
        .expect("hedge ledger mutex poisoned")
        .budget
        .try_charge(charge)
    {
        return None;
    }

    let hedge_provider = claim.provider();
    let progress = Arc::new(AtomicU64::new(0));
    let on_bytes = opts.delivered.as_ref().map(|sink| {
        let sink = Arc::clone(sink);
        let key = item.key.clone();
        let who = *hedge_provider.as_bytes();
        Arc::new(move |d: u64| sink(&key, who, d)) as Arc<dyn Fn(u64) + Send + Sync>
    });
    let fut: BoxedTransfer = Box::pin(transfer_once(
        dialer.clone(),
        remote.clone(),
        hedge_provider,
        request,
        opts.stall_hard_limit,
        Arc::clone(&progress),
        on_bytes,
    ));
    {
        let mut l = ledger.lock().expect("hedge ledger mutex poisoned");
        l.hedges = l.hedges.saturating_add(1);
    }
    (opts.telemetry)(ProviderEvent::Trying(*hedge_provider.as_bytes()));
    tracing::debug!(
        child = index,
        child_hash = %hash,
        provider = %primary.fmt_short(),
        hedge_provider = %hedge_provider.fmt_short(),
        missing_bytes,
        expected_ms = expected.as_millis() as u64,
        p95_ms = p95.unwrap_or(Duration::ZERO).as_millis() as u64,
        bytes = charge,
        "swarm hedge armed"
    );
    Some(HedgeRun {
        provider: hedge_provider,
        fut,
        progress,
        charge,
        split_rel: split.split_rel,
        armed_at,
        started: Instant::now(),
        _claim: claim,
    })
}

/// One `execute_get` with a progress watchdog.
///
/// Returns as soon as the request completes, errors, or goes
/// `stall_hard_limit` without its `bytes_read` growing. `progress` mirrors the
/// running payload-byte count so a caller that drops this future can still see
/// how far it got. Takes its dialer and remote by value so the future is
/// `'static` and can be boxed beside a sibling in a `select!`. In the stall case the
/// `GetProgress` stream is dropped on the way out, which resets the QUIC stream
/// — see the module doc's cancellation note for why that is enough.
///
/// `on_bytes`, when set, is fed each round's exact byte delta as it is
/// observed, plus the tail on `Done` — the sum equals `payload_bytes_read`
/// (the [`DeliveredSink`] plumbing; `None` for every caller with no receive
/// feed to keep).
async fn transfer_once(
    dialer: Dialer,
    remote: Remote,
    provider: EndpointId,
    request: GetRequest,
    stall_hard_limit: Duration,
    progress: Arc<AtomicU64>,
    on_bytes: Option<Arc<dyn Fn(u64) + Send + Sync>>,
) -> std::result::Result<Stats, TransferFault> {
    let conn = dial(&dialer, provider).await?;

    // The `PooledConn` keeps the collab connection from idling out; it stays
    // alive for the whole transfer and is dropped with this function, leaving
    // the connection warm.
    let get = remote.execute_get(conn.conn.clone(), request);
    let mut stream = std::pin::pin!(get.stream());

    let mut last_bytes = 0u64;
    let mut last_growth = Instant::now();
    loop {
        let slice = stall_hard_limit
            .saturating_sub(last_growth.elapsed())
            .max(MIN_WATCHDOG_SLICE);
        match tokio::time::timeout(slice, stream.next()).await {
            Ok(Some(GetProgressItem::Progress(b))) => {
                if b > last_bytes {
                    if let Some(f) = &on_bytes {
                        f(b - last_bytes);
                    }
                    last_bytes = b;
                    last_growth = Instant::now();
                    // Published so the caller can still read this assignment's
                    // progress after it has CANCELLED it — a cancelled future
                    // returns nothing, and the refund needs to know how much of
                    // the charged range the loser actually moved.
                    progress.store(b, Ordering::Relaxed);
                }
            }
            Ok(Some(GetProgressItem::Done(stats))) => {
                if let Some(f) = &on_bytes {
                    let tail = stats.payload_bytes_read.saturating_sub(last_bytes);
                    if tail > 0 {
                        f(tail);
                    }
                }
                return Ok(stats);
            }
            Ok(Some(GetProgressItem::Error(e))) => {
                return Err(classify_get_error(&e, last_bytes, provider));
            }
            Ok(None) => {
                return Err(TransferFault::Failed {
                    bytes: last_bytes,
                    error: anyhow::anyhow!(
                        "get stream from {} ended without a terminal item",
                        provider.fmt_short()
                    ),
                })
            }
            Err(_elapsed) => {
                if last_growth.elapsed() >= stall_hard_limit {
                    return Err(TransferFault::Stalled { bytes: last_bytes });
                }
            }
        }
    }
}

/// A connection to `provider` for one transfer.
async fn dial(
    dialer: &Dialer,
    provider: EndpointId,
) -> std::result::Result<PooledConn, TransferFault> {
    let Dialer { pool, addrs } = dialer;
    let Some(addr) = addrs(&provider) else {
        return Err(TransferFault::DialFailed {
            error: anyhow::anyhow!("no dial address for {}", provider.fmt_short()),
        });
    };
    // The pool logs the failure itself (`collab dial failed`).
    pool.get(addr).await.map_err(|e| TransferFault::DialFailed {
        error: anyhow::anyhow!("dial {}: {e}", provider.fmt_short()),
    })
}

/// Pick a provider and reserve a slot on it, under ONE lock.
///
/// Picking and incrementing must be atomic together: two children that both
/// observe an idle provider before either increments would both pile onto it,
/// which is exactly the imbalance the least-loaded rule exists to prevent.
fn claim_provider(
    states: &States,
    providers: &[EndpointId],
    forced: Option<EndpointId>,
) -> Option<InflightGuard> {
    let chosen = {
        let mut guard = states.lock().expect("assignment states mutex poisoned");
        // A forced pick (the winner of a hedge) skips ranking but NOT the
        // backoff gate: a provider that has since been evicted is not a
        // sensible place to send the rest of the child.
        let now = Instant::now();
        let forced = forced.filter(|p| guard.get(p).is_some_and(|st| st.available(now)));
        let chosen = match forced {
            Some(p) => p,
            None => pick_provider(&guard, providers, now)?,
        };
        // A live provider that appeared after the run began has no state
        // yet: it starts from the default.
        guard.entry(chosen).or_default().inflight += 1;
        chosen
    };
    Some(InflightGuard {
        states: Arc::clone(states),
        provider: chosen,
    })
}

/// A2's provider choice: among the providers not in backoff (nor busy), the
/// least loaded; ties go to the fewest failures, then to list order. A
/// provider with no state yet (a live one that just appeared) is idle.
///
/// Deliberately dumb and deliberately alone in a function — A4's
/// `RankedProviders` (goodput EWMA, SRTT, exploration share) replaces this body
/// and nothing else. D4 §9: ranking may only influence ORDER, never a static
/// partition, so the loop around it stays work-stealing whatever this returns.
fn pick_provider(
    states: &HashMap<EndpointId, ProviderState>,
    providers: &[EndpointId],
    now: Instant,
) -> Option<EndpointId> {
    providers
        .iter()
        .enumerate()
        .filter_map(|(order, id)| {
            let (inflight, failures) = match states.get(id) {
                Some(st) if !st.available(now) => return None,
                Some(st) => (st.inflight, st.failures),
                None => (0, 0),
            };
            Some((inflight, failures, order, *id))
        })
        .min()
        .map(|(_, _, _, id)| id)
}

/// How long until the earliest of `providers` leaves backoff.
///
/// Scoped to the ITEM's own providers, never the run-wide union: under
/// per-item provider lists a sibling's provider whose backoff has already
/// elapsed would otherwise read as "wait 0", and an item whose only holder is
/// parked would burn every round of its ladder in a few milliseconds.
fn earliest_wait(states: &States, providers: &[EndpointId]) -> Duration {
    let now = Instant::now();
    let guard = states.lock().expect("assignment states mutex poisoned");
    providers
        .iter()
        .filter_map(|p| guard.get(p)?.available_at())
        .map(|t| t.saturating_duration_since(now))
        .min()
        .unwrap_or(BACKOFF_BASE)
        .max(MIN_BACKOFF_SLEEP)
}

/// Whether some of `providers` is parked only because it is BUSY
/// (`ERR_LIMIT`), not in failure backoff — a wait that spends no round.
fn waiting_on_busy(states: &States, providers: &[EndpointId]) -> bool {
    let now = Instant::now();
    let guard = states.lock().expect("assignment states mutex poisoned");
    providers.iter().any(|p| {
        guard.get(p).is_some_and(|st| {
            st.busy_until.is_some_and(|t| t > now) && !st.next_try.is_some_and(|t| t > now)
        })
    })
}

/// Park `provider` until `until` after an `ERR_LIMIT` refusal: no strike,
/// the consecutive-failure count and the backoff ladder are untouched.
fn mark_busy(states: &States, provider: EndpointId, bytes: u64, elapsed: Duration, until: Instant) {
    let mut guard = states.lock().expect("assignment states mutex poisoned");
    let st = guard.entry(provider).or_default();
    st.stats.bytes += bytes;
    st.stats.elapsed += elapsed;
    st.busy_until = Some(st.busy_until.map_or(until, |t| t.max(until)));
}

/// Credit an attempt that ended without a strike (a refusal) with the bytes
/// and time it took.
fn credit_attempt(states: &States, provider: EndpointId, bytes: u64, elapsed: Duration) {
    let mut guard = states.lock().expect("assignment states mutex poisoned");
    let st = guard.entry(provider).or_default();
    st.stats.bytes += bytes;
    st.stats.elapsed += elapsed;
}

/// Record a completed transfer against its provider.
///
/// `completed_child` carries ruling R3: the child belongs to whichever transfer
/// made it COMPLETE, whoever that was. A primary that finishes the whole
/// missing range qualifies; so does a hedge whose back half happened to be the
/// last of the child, and so does the follow-up round that picks up a
/// remainder. Every success feeds `bytes`, the goodput EWMA and the backoff
/// reset — but `children` moves exactly once per child, or
/// `AssignmentReport::total_children` stops meaning "children fetched", in
/// either direction: double-counting a hedged child inflates it, and the
/// earlier rule of never crediting a hedge lost the ones a hedge finished
/// outright, because the round that would have credited them returned through
/// the `is_complete` early return without recording anything.
fn record_success(
    states: &States,
    provider: EndpointId,
    stats: &Stats,
    elapsed: Duration,
    completed_child: bool,
) {
    let mut guard = states.lock().expect("assignment states mutex poisoned");
    if let Some(st) = guard.get_mut(&provider) {
        // D4 §6: a success resets the consecutive-failure count AND clears the
        // backoff, so one good transfer readmits a provider immediately.
        st.failures = 0;
        st.next_try = None;
        st.stats.bytes += stats.payload_bytes_read;
        if completed_child {
            st.stats.children += 1;
        }
        st.stats.elapsed += elapsed;
        // Goodput EWMA, warm-started so the first samples are not diluted by a
        // cold value: α = min(1/n, GOODPUT_ALPHA), n counting this sample.
        let secs = elapsed.as_secs_f64().max(1e-6);
        let sample = stats.payload_bytes_read as f64 / secs;
        st.goodput_samples = st.goodput_samples.saturating_add(1);
        st.goodput = Some(match st.goodput {
            None => sample,
            Some(prev) => {
                let alpha = (1.0 / st.goodput_samples as f64).min(GOODPUT_ALPHA);
                prev * (1.0 - alpha) + sample * alpha
            }
        });
    }
}

/// This provider's smoothed goodput, or — for one that has completed nothing —
/// the median of the providers that have measured one.
///
/// D4 §4.5 is explicit that an entirely unmeasured swarm does NOT hedge: with
/// no idea what "fast" means here, every assignment would look late.
fn ewma_goodput(states: &States, provider: EndpointId) -> Option<f64> {
    let guard = states.lock().expect("assignment states mutex poisoned");
    if let Some(g) = guard.get(&provider).and_then(|st| st.goodput) {
        return Some(g);
    }
    let mut measured: Vec<f64> = guard.values().filter_map(|st| st.goodput).collect();
    if measured.is_empty() {
        return None;
    }
    measured.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(measured[measured.len() / 2])
}

/// Record a failed attempt and, if this provider has now failed
/// [`EVICT_AFTER_FAILURES`] times in a row, put it in backoff.
///
/// The escalation is guarded to ONE step per backoff window. Children run
/// concurrently, so a provider that is down gets a failure from each of the
/// children currently assigned to it within milliseconds; letting every one of
/// them double the rung would jump a two-provider swarm from "500 ms" to
/// "16 s" on its first round and turn the bounded ladder into a stall. The
/// report's cumulative `stats.failures` still counts every attempt.
///
/// Thin wrapper over [`record_failure_at`] with the real wall clock — see its
/// doc for why the split exists.
fn record_failure(states: &States, provider: EndpointId, fault: &TransferFault, elapsed: Duration) {
    record_failure_at(states, provider, fault, elapsed, Instant::now());
}

/// [`record_failure`]'s body, with `now` a parameter instead of a fresh
/// `Instant::now()` read.
///
/// The seam a test needs (Wave 1 final review item 3): a test asserting the
/// backoff ladder's exact rung durations by reading `next_try` back against
/// its OWN `Instant::now()` call is comparing two different clock reads
/// separated by however long the test body took to run between them —
/// ordinarily negligible, but not zero, and not bounded on a saturated CI
/// runner. Passing the same `now` in both directions makes the rung duration
/// an exact arithmetic fact (`next_try - now == BACKOFF_BASE * 2^rung`)
/// rather than a measurement.
fn record_failure_at(
    states: &States,
    provider: EndpointId,
    fault: &TransferFault,
    elapsed: Duration,
    now: Instant,
) {
    let mut guard = states.lock().expect("assignment states mutex poisoned");
    let Some(st) = guard.get_mut(&provider) else {
        return;
    };
    st.stats.failures += 1;
    st.stats.bytes += fault.bytes();
    st.stats.elapsed += elapsed;
    if fault.is_stall() {
        st.stalls += 1;
    } else if let Some(error) = fault.error() {
        st.last_error = Some(format!("{error:#}"));
        st.last_error_at = Some(now);
    }
    if st.next_try.is_some_and(|t| t > now) {
        return; // another child already escalated this window
    }
    st.failures += 1;
    if st.failures >= EVICT_AFTER_FAILURES {
        let rung = (st.failures - EVICT_AFTER_FAILURES).min(BACKOFF_MAX_RUNGS - 1);
        st.next_try = Some(now + BACKOFF_BASE * 2u32.pow(rung));
    }
}

fn report_from_with_ledger(states: &States, ledger: &Ledger) -> AssignmentReport {
    let mut report = report_from(states);
    let l = ledger.lock().expect("hedge ledger mutex poisoned");
    report.hedges = l.hedges;
    report.hedge_bytes = l.hedge_bytes;
    report
}

fn report_from(states: &States) -> AssignmentReport {
    let guard = states.lock().expect("assignment states mutex poisoned");
    let mut report = AssignmentReport::default();
    for (id, st) in guard.iter() {
        report.per_provider.insert(*id, st.stats.clone());
    }
    // `stalls` is swarm-wide and counted separately from `stats.failures`,
    // which covers stalls AND errors: the two are reported beside each other so
    // "the provider went useless" is distinguishable from "the provider broke".
    report.stalls = guard.values().map(|st| st.stalls).sum();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distinct, WELL-FORMED endpoint id. `[n; 32]` will not do: an
    /// `EndpointId` is an ed25519 public key and `from_bytes` verifies the
    /// point, so a constant-byte array is rejected. Takes no tag — each call
    /// already generates a fresh key, so a caller-supplied number never
    /// distinguished anything.
    fn distinct_id() -> EndpointId {
        iroh::SecretKey::generate().public()
    }

    fn states_of(entries: Vec<(EndpointId, ProviderState)>) -> HashMap<EndpointId, ProviderState> {
        entries.into_iter().collect()
    }

    #[test]
    fn pick_provider_prefers_the_least_loaded_then_fewest_failures_then_order() {
        let (a, b, c) = (distinct_id(), distinct_id(), distinct_id());
        let now = Instant::now();

        // Least loaded wins outright.
        let st = states_of(vec![
            (
                a,
                ProviderState {
                    inflight: 3,
                    ..Default::default()
                },
            ),
            (
                b,
                ProviderState {
                    inflight: 1,
                    ..Default::default()
                },
            ),
        ]);
        assert_eq!(pick_provider(&st, &[a, b], now), Some(b));

        // Equal load ⇒ fewest failures.
        let st = states_of(vec![
            (
                a,
                ProviderState {
                    inflight: 2,
                    failures: 2,
                    ..Default::default()
                },
            ),
            (
                b,
                ProviderState {
                    inflight: 2,
                    failures: 0,
                    ..Default::default()
                },
            ),
        ]);
        assert_eq!(pick_provider(&st, &[a, b], now), Some(b));

        // Equal load AND equal failures ⇒ list order, not map order.
        let st = states_of(vec![
            (a, ProviderState::default()),
            (b, ProviderState::default()),
            (c, ProviderState::default()),
        ]);
        assert_eq!(pick_provider(&st, &[c, b, a], now), Some(c));
    }

    #[test]
    fn pick_provider_skips_backoff_and_returns_none_when_every_provider_is_parked() {
        let (a, b) = (distinct_id(), distinct_id());
        let now = Instant::now();
        let st = states_of(vec![
            (
                a,
                ProviderState {
                    next_try: Some(now + Duration::from_secs(5)),
                    ..Default::default()
                },
            ),
            (
                b,
                ProviderState {
                    inflight: 9,
                    ..Default::default()
                },
            ),
        ]);
        // The parked provider is skipped even though it is idle.
        assert_eq!(pick_provider(&st, &[a, b], now), Some(b));

        let st = states_of(vec![
            (
                a,
                ProviderState {
                    next_try: Some(now + Duration::from_secs(5)),
                    ..Default::default()
                },
            ),
            (
                b,
                ProviderState {
                    next_try: Some(now + Duration::from_secs(1)),
                    ..Default::default()
                },
            ),
        ]);
        assert_eq!(
            pick_provider(&st, &[a, b], now),
            None,
            "every provider parked ⇒ no pick, so the caller waits instead of spinning"
        );

        // An elapsed backoff is not a backoff.
        let st = states_of(vec![(
            a,
            ProviderState {
                next_try: Some(now - Duration::from_millis(1)),
                ..Default::default()
            },
        )]);
        assert_eq!(pick_provider(&st, &[a], now), Some(a));
    }

    #[test]
    fn backoff_escalates_once_per_window_and_a_success_clears_it() {
        let a = distinct_id();
        let states: States = Arc::new(Mutex::new(states_of(vec![(a, ProviderState::default())])));
        let fault = || TransferFault::Stalled { bytes: 7 };

        // Two failures: counted, no backoff yet.
        record_failure(&states, a, &fault(), Duration::from_millis(1));
        record_failure(&states, a, &fault(), Duration::from_millis(1));
        assert!(states.lock().unwrap()[&a].next_try.is_none());

        // The third parks it at the first rung.
        record_failure(&states, a, &fault(), Duration::from_millis(1));
        let parked_at = states.lock().unwrap()[&a].next_try.expect("parked");
        assert!(parked_at > Instant::now());

        // Concurrent siblings failing inside the same window must NOT escalate:
        // the rung, and therefore the deadline, is unchanged.
        for _ in 0..10 {
            record_failure(&states, a, &fault(), Duration::from_millis(1));
        }
        {
            let g = states.lock().unwrap();
            assert_eq!(
                g[&a].next_try,
                Some(parked_at),
                "ten siblings failing in one window must not ratchet the ladder ten rungs"
            );
            assert_eq!(g[&a].failures, 3, "consecutive-failure count is per window");
            assert_eq!(g[&a].stats.failures, 13, "the report counts every attempt");
            assert_eq!(
                g[&a].stats.bytes,
                13 * 7,
                "partial bytes are still credited"
            );
            assert_eq!(
                g[&a].stalls, 13,
                "every stall is counted, window guard or not"
            );
        }

        // One success readmits it immediately.
        record_success(
            &states,
            a,
            &Stats::default(),
            Duration::from_millis(1),
            true,
        );
        let g = states.lock().unwrap();
        assert!(g[&a].next_try.is_none(), "a success clears the backoff");
        assert_eq!(g[&a].failures, 0, "and resets the consecutive count");
        assert_eq!(g[&a].stats.children, 1);
    }

    /// The bucket starts FULL, refuses below half, refuses a charge it cannot
    /// cover, refunds, and never earns past the cap.
    #[test]
    fn hedge_budget_starts_full_refuses_below_half_and_refunds() {
        // 1 MiB collection ⇒ cap = 52 428.8 tokens (5 %).
        let total = 1024 * 1024u64;
        let cap = HEDGE_BUDGET_RATIO * total as f64;
        let mut b = HedgeBudget::new(total);
        assert_eq!(b.tokens, cap, "a token bucket starts full");

        // A charge just under half the cap leaves it above half ⇒ admitted.
        assert!(
            b.try_charge((cap * 0.4) as u64),
            "the first hedge is payable"
        );
        assert!(b.tokens > cap / 2.0);

        // The next one would take it below half — the half rule is checked
        // BEFORE the charge, so this is admitted and lands under half...
        assert!(b.try_charge((cap * 0.4) as u64));
        assert!(b.tokens < cap / 2.0);
        // ...and now nothing more is admitted, however small.
        assert!(
            !b.try_charge(1),
            "below half the cap the bucket refuses even a one-byte hedge — that \
             is the storm brake, not the exhaustion check"
        );

        // A refund puts it back over half and reopens hedging.
        b.refund((cap * 0.4) as u64);
        assert!(b.tokens > cap / 2.0);
        assert!(b.try_charge(1), "a refund reopens the gate");

        // Earning never exceeds the cap.
        b.earn(u64::MAX / 2);
        assert_eq!(b.tokens, cap, "the bucket cannot earn past its cap");

        // And a charge bigger than the whole bucket is refused outright.
        assert!(
            !b.try_charge((cap * 2.0) as u64),
            "a hedge the budget cannot cover is refused even with a full bucket"
        );
    }

    /// A package too small for its own hedge can never hedge: the cap is a
    /// fraction of the COLLECTION, so one child's half-range can exceed it.
    #[test]
    fn hedge_budget_refuses_when_one_hedge_exceeds_the_whole_cap() {
        // 6 children of 512 KiB ⇒ cap = 157 286; half a child = 262 144.
        let total = 6 * 512 * 1024u64;
        let mut b = HedgeBudget::new(total);
        assert!(
            !b.try_charge(256 * 1024),
            "half a 512 KiB child is 262 144 bytes against a 157 286 cap — no \
             hedge on this package is ever affordable"
        );
    }

    /// The midpoint cut works on the RANGES, not on the blob's end, and the
    /// offset it reports is request-relative — the two things findings 2 and 3
    /// were about.
    #[test]
    fn split_missing_cuts_the_ranges_not_the_blob_and_reports_an_arm_time_offset() {
        const SIZE: u64 = 64 * CHUNK_BYTES;

        // Contiguous whole blob: an even cut, and the front offset is half.
        let whole = ChunkRanges::chunks(..64u64);
        let s = split_missing_at_midpoint(&whole, SIZE).expect("splittable");
        assert_eq!(s.charge, 32 * CHUNK_BYTES);
        assert_eq!(s.split_rel, 32 * CHUNK_BYTES);

        // The shape a cancelled hedge leaves: [0,8) already present, so the
        // missing set is [8,32) u [48,64) — 40 chunks in two spans. The charge
        // must be 20 chunks (half the MISSING bytes), NOT `size - split`, which
        // would bill the 16 present chunks in between.
        let gapped = ChunkRanges::chunks(8u64..32) | ChunkRanges::chunks(48u64..64);
        let s = split_missing_at_midpoint(&gapped, SIZE).expect("splittable");
        assert_eq!(
            s.charge,
            20 * CHUNK_BYTES,
            "the charge is the byte extent of the BACK RANGES"
        );
        assert_eq!(
            s.split_rel,
            20 * CHUNK_BYTES,
            "and the offset counts missing bytes only — it is what the \
             primary's own progress counter is measured in"
        );
        // And the back half really is the tail of the missing set.
        assert!(
            s.back.is_subset(&gapped),
            "the hedge must never ask for bytes that are not missing"
        );
        assert!(!s.back.is_empty());

        // Degenerate remainders are not worth racing.
        assert!(split_missing_at_midpoint(&ChunkRanges::empty(), SIZE).is_none());
        assert!(
            split_missing_at_midpoint(&ChunkRanges::chunks(0u64..1), SIZE).is_none(),
            "one chunk left cannot be cut into two non-empty halves"
        );
    }

    /// The bucket admits at EXACTLY half the cap (ruling R1): that is where two
    /// providers splitting a collection evenly land, and a strict `>` refused
    /// every hedge there.
    #[test]
    fn hedge_budget_admits_at_exactly_half_the_cap() {
        let total = 1024 * 1024u64;
        let cap = HEDGE_BUDGET_RATIO * total as f64;
        let mut b = HedgeBudget::new(total);
        b.tokens = cap / 2.0;
        assert!(
            b.try_charge(1),
            "tokens sitting exactly on cap/2 must still admit a hedge"
        );
    }

    #[test]
    fn completion_window_keeps_the_last_32_and_reports_the_nearest_rank_p95() {
        let mut w = CompletionWindow::default();
        assert_eq!(w.p95(), None, "an empty window has no opinion");

        w.record(Duration::from_millis(7));
        assert_eq!(
            w.p95(),
            Some(Duration::from_millis(7)),
            "one sample IS its own p95 — the honest answer for n = 1"
        );

        // 1..=100 ms, so only the last 32 (69..=100) survive; the nearest-rank
        // p95 of 32 sorted samples is index ceil(0.95*32)-1 = 30 ⇒ 99 ms.
        let mut w = CompletionWindow::default();
        for ms in 1..=100u64 {
            w.record(Duration::from_millis(ms));
        }
        assert_eq!(w.0.len(), HEDGE_COMPLETION_WINDOW);
        assert_eq!(w.p95(), Some(Duration::from_millis(99)));
        assert!(
            !w.0.contains(&Duration::from_millis(68)),
            "samples older than the window must be gone, not merely outvoted"
        );
    }

    /// Goodput is smoothed per provider, and a provider that has completed
    /// nothing borrows the median of those that have — with no measurement
    /// anywhere, there is no hedge (D4 §4.5).
    #[test]
    fn goodput_ewma_warms_up_and_falls_back_to_the_median() {
        let (a, b, c) = (distinct_id(), distinct_id(), distinct_id());
        let states: States = Arc::new(Mutex::new(states_of(vec![
            (a, ProviderState::default()),
            (b, ProviderState::default()),
            (c, ProviderState::default()),
        ])));
        assert_eq!(
            ewma_goodput(&states, a),
            None,
            "an unmeasured swarm does not hedge"
        );

        // a: 1000 B in 1 s ⇒ 1000 B/s on the first sample (no cold start).
        let mut stats = Stats::default();
        stats.counters.payload_bytes_read = 1000;
        record_success(&states, a, &stats, Duration::from_secs(1), true);
        assert_eq!(ewma_goodput(&states, a), Some(1000.0));

        // A second, much faster sample moves it by α = min(1/2, 0.25) = 0.25.
        let mut fast = Stats::default();
        fast.counters.payload_bytes_read = 5000;
        record_success(&states, a, &fast, Duration::from_secs(1), true);
        let g = ewma_goodput(&states, a).unwrap();
        assert!(
            (g - (1000.0 * 0.75 + 5000.0 * 0.25)).abs() < 1e-6,
            "the EWMA must smooth, not jump: {g}"
        );

        // b measured, c not ⇒ c borrows the median of {a, b}.
        record_success(&states, b, &stats, Duration::from_secs(1), true);
        let median = ewma_goodput(&states, c).expect("c borrows a median");
        assert!(
            median == 1000.0 || (median - g).abs() < 1e-6,
            "the fallback must be one of the measured values, got {median}"
        );
    }

    /// An item waits for ITS OWN providers' backoff, not for a sibling's
    /// already-elapsed one — otherwise a parked sole holder spins its ladder.
    #[test]
    fn earliest_wait_is_scoped_to_the_items_providers() {
        let (own, other) = (distinct_id(), distinct_id());
        let now = Instant::now();
        let states: States = Arc::new(Mutex::new(states_of(vec![
            (
                own,
                ProviderState {
                    next_try: Some(now + Duration::from_secs(2)),
                    ..Default::default()
                },
            ),
            (
                other,
                ProviderState {
                    next_try: Some(now - Duration::from_secs(1)),
                    ..Default::default()
                },
            ),
        ])));
        let wait = earliest_wait(&states, &[own]);
        assert!(
            wait > Duration::from_millis(1500) && wait <= Duration::from_secs(2),
            "the item waits for its own parked provider, not a sibling's elapsed backoff: {wait:?}"
        );
        // The union still sees the elapsed backoff — floored, never zero.
        assert_eq!(earliest_wait(&states, &[own, other]), MIN_BACKOFF_SLEEP);
        assert_eq!(earliest_wait(&states, &[other]), MIN_BACKOFF_SLEEP);
    }

    #[test]
    fn backoff_rungs_double_and_stop_at_the_cap() {
        let a = distinct_id();
        let states: States = Arc::new(Mutex::new(states_of(vec![(a, ProviderState::default())])));
        let fault = || TransferFault::Failed {
            bytes: 0,
            error: anyhow::anyhow!("dead"),
        };
        // ONE wall-clock read for the whole test (Wave 1 final review item 3):
        // every `now` handed to `record_failure_at` is `base` plus pure
        // arithmetic, and the rung is read back against that SAME `now` —
        // `next_try - now` is then exactly `BACKOFF_BASE * 2^rung`, not a
        // measurement across two separate `Instant::now()` calls that a
        // loaded CI runner could stretch apart.
        let base = Instant::now();
        let mut seen = Vec::new();
        for i in 0..(EVICT_AFTER_FAILURES + BACKOFF_MAX_RUNGS + 3) {
            // Force each failure into its own window by clearing the deadline,
            // which is what elapsing it does in the real loop.
            states.lock().unwrap().get_mut(&a).unwrap().next_try = None;
            // Strictly increasing per call so `next_try.is_some_and(|t| t >
            // now)` above never spuriously re-fires the "already escalated
            // this window" guard against a `now` that has not advanced.
            let now = base + Duration::from_secs(i as u64 * 60);
            record_failure_at(&states, a, &fault(), Duration::ZERO, now);
            if let Some(t) = states.lock().unwrap()[&a].next_try {
                seen.push(t.saturating_duration_since(now).as_millis() as u64);
            }
        }
        // 500, 1000, 2000, 4000, 8000, 16000, then flat at the cap — exact,
        // no rounding needed now that both ends of the subtraction share one
        // `now`.
        let rungs = seen;
        assert_eq!(
            &rungs[..6],
            &[500, 1000, 2000, 4000, 8000, 16000],
            "the ladder doubles from BACKOFF_BASE"
        );
        assert!(
            rungs[6..].iter().all(|ms| *ms == 16000),
            "and stops at {BACKOFF_MAX_RUNGS} rungs instead of growing forever: {rungs:?}"
        );
    }

    /// A provider that appeared in a live set after the run began has no
    /// state yet: it is picked as idle. A busy provider is skipped like a
    /// parked one.
    #[test]
    fn pick_provider_treats_an_unseen_provider_as_idle_and_skips_a_busy_one() {
        let (seen, unseen, busy) = (distinct_id(), distinct_id(), distinct_id());
        let now = Instant::now();
        let st = states_of(vec![
            (
                seen,
                ProviderState {
                    inflight: 1,
                    ..Default::default()
                },
            ),
            (
                busy,
                ProviderState {
                    busy_until: Some(now + LIMIT_RETRY),
                    ..Default::default()
                },
            ),
        ]);
        assert_eq!(pick_provider(&st, &[seen, unseen], now), Some(unseen));
        assert_eq!(pick_provider(&st, &[busy, seen], now), Some(seen));
        assert_eq!(pick_provider(&st, &[busy], now), None);
    }

    /// `ERR_LIMIT` parks a provider for [`LIMIT_RETRY`] without a strike:
    /// the consecutive count and the ladder are untouched, and a wait on it
    /// is a busy wait (no round spent).
    #[test]
    fn a_busy_provider_is_parked_without_a_strike() {
        let a = distinct_id();
        let states: States = Arc::new(Mutex::new(states_of(vec![(a, ProviderState::default())])));
        mark_busy(&states, a, 0, Duration::ZERO, Instant::now() + LIMIT_RETRY);
        {
            let g = states.lock().unwrap();
            assert_eq!(g[&a].failures, 0, "no strike");
            assert_eq!(g[&a].stats.failures, 0, "not a failure in the report");
            assert!(g[&a].next_try.is_none(), "the ladder is untouched");
            assert!(g[&a].busy_until.is_some());
        }
        assert!(waiting_on_busy(&states, &[a]));
        let wait = earliest_wait(&states, &[a]);
        assert!(
            wait > LIMIT_RETRY - Duration::from_millis(500) && wait <= LIMIT_RETRY,
            "{wait:?}"
        );
        // A provider parked by the LADDER is not a busy wait, busy or not.
        states.lock().unwrap().get_mut(&a).unwrap().next_try =
            Some(Instant::now() + Duration::from_secs(5));
        assert!(!waiting_on_busy(&states, &[a]));
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// A live run's cap follows the items in flight — the larger of 5 % of their
    /// bytes and half the largest — growing with each item taken and shrinking
    /// as each ends; the tokens never exceed it.
    #[test]
    fn a_live_budget_follows_the_items_in_flight() {
        let mut b = HedgeBudget::live();
        assert!(close(b.cap, 0.0) && close(b.tokens, 0.0), "empty at the start");
        b.extend(1_000_000);
        assert!(close(b.cap, 500_000.0), "one hedge of the item: {}", b.cap);
        assert!(close(b.tokens, 500_000.0), "an item brings its share");
        b.extend(3_000_000);
        // 5 % of 4 MB is 200 kB; half of the 3 MB item is 1.5 MB.
        assert!(close(b.cap, 1_500_000.0), "{}", b.cap);
        assert!(close(b.tokens, 1_500_000.0), "the cap's increase joins: {}", b.tokens);
        assert!(b.try_charge(60_000));
        b.retire(3_000_000);
        assert!(close(b.cap, 500_000.0), "the floor drops to the 1 MB item: {}", b.cap);
        assert!(b.tokens <= b.cap, "the tokens follow the cap down");
        b.retire(1_000_000);
        assert!(close(b.cap, 0.0), "nothing in flight, no budget: {}", b.cap);
        assert!(b.tokens <= b.cap);
    }

    /// One item in flight: a full bucket pays for one hedge of half that item, a
    /// second identical one waits for a refund.
    #[test]
    fn a_live_budget_admits_one_hedge_of_a_lone_item() {
        let mut b = HedgeBudget::live();
        b.extend(1_000_000);
        assert!(b.try_charge(500_000), "half the item, from a full bucket");
        assert!(!b.try_charge(500_000), "a second one is refused");
        b.refund(500_000);
        assert!(b.try_charge(500_000), "the refund pays for it again");
        assert!(!b.try_charge(500_001), "more than half the item is never payable");
    }

    /// The cap is the larger of the ratio term and the floor, and the crossover
    /// sits where the in-flight bytes are ten times the largest item.
    #[test]
    fn a_live_budget_cap_is_the_larger_of_the_ratio_and_the_floor() {
        let mut b = HedgeBudget::live();
        for _ in 0..10 {
            b.extend(1_000_000);
            assert!(close(b.cap, 500_000.0), "the floor holds up to ten items: {}", b.cap);
            assert!(close(b.tokens, 500_000.0), "no increase, no new tokens");
        }
        // The eleventh item takes the ratio term (550 kB) past the floor.
        b.extend(1_000_000);
        assert!(close(b.cap, 550_000.0), "{}", b.cap);
        assert!(close(b.tokens, 550_000.0), "{}", b.tokens);

        // Many small items and one large: the floor wins, then the ratio again.
        let mut b = HedgeBudget::live();
        for _ in 0..100 {
            b.extend(100_000);
        }
        assert!(close(b.cap, 500_000.0), "ratio term, 5 % of 10 MB: {}", b.cap);
        b.extend(10_000_000);
        assert!(close(b.cap, 5_000_000.0), "floor term, half of 10 MB: {}", b.cap);
        b.retire(10_000_000);
        assert!(close(b.cap, 500_000.0), "back to the ratio term: {}", b.cap);
        assert!(b.tokens <= b.cap);
    }

    /// Retiring the largest item drops the floor to the next largest and clamps
    /// the tokens to the new cap.
    #[test]
    fn a_live_budget_retiring_the_largest_item_lowers_the_floor() {
        let mut b = HedgeBudget::live();
        b.extend(4_000_000);
        b.extend(1_000_000);
        b.extend(1_000_000);
        assert!(close(b.cap, 2_000_000.0), "{}", b.cap);
        assert!(b.try_charge(1_000_000));
        assert!(close(b.tokens, 1_000_000.0), "{}", b.tokens);
        b.retire(4_000_000);
        assert!(close(b.cap, 500_000.0), "half of the next largest: {}", b.cap);
        assert!(close(b.tokens, 500_000.0), "clamped to the new cap: {}", b.tokens);
    }

    /// With nothing in flight there is no budget and nothing is admitted.
    #[test]
    fn a_live_budget_with_nothing_in_flight_admits_nothing() {
        let mut b = HedgeBudget::live();
        assert!(!b.try_charge(1), "an empty budget admits nothing");
        b.extend(2_000_000);
        b.retire(2_000_000);
        assert!(close(b.cap, 0.0) && close(b.tokens, 0.0));
        assert!(!b.try_charge(1), "a drained run admits nothing");
        b.refund(1_000);
        b.earn(1_000_000);
        assert!(close(b.tokens, 0.0), "nothing can be earned past a zero cap");
    }

    /// Items of equal size retire one at a time.
    #[test]
    fn a_live_budget_counts_equal_sized_items_separately() {
        let mut b = HedgeBudget::live();
        b.extend(1_000_000);
        b.extend(1_000_000);
        b.extend(100_000);
        assert!(close(b.cap, 500_000.0), "{}", b.cap);
        b.retire(1_000_000);
        assert!(close(b.cap, 500_000.0), "one 1 MB item is still in flight: {}", b.cap);
        b.retire(1_000_000);
        assert!(close(b.cap, 50_000.0), "only the 100 kB item is left: {}", b.cap);
        assert!(b.tokens <= b.cap);
    }
}
