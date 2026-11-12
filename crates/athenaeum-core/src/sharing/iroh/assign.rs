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
//! [`pick_provider`] sends an item where it should finish first: the cost of a
//! provider is `(in flight + 1) / uplink`, its streams over what it delivers
//! across all of them. The uplink is METERED, not sampled ([`UplinkMeter`]):
//! the bytes of every attempt on the provider, finished or still running, over
//! the time it had work, read in windows of [`UPLINK_WINDOW`] that the run
//! closes on a tick. So a provider that has just gone slow stops getting items
//! within about a window, before any of its attempts ends, and one whose items
//! are always rescued by a hedge is measured like any other. A hedge's provider
//! is picked the same way. An idle provider's old reading recovers toward the
//! best known one over [`UPLINK_RECOVERY`], which hands a slow provider one
//! probe item now and then. With nobody measured the pick is the least-loaded
//! one. It is ONE function, so the ranking is one edit. A [`ProviderSet`] may
//! be live: a provider that appears is picked up at the item's next round, and
//! an item with no provider sleeps until one appears.
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
//! largest one. A hedge's charge stays outstanding until it settles and what it
//! duplicated is owed back at 5 % of the bytes of completing rounds, so a budget with nothing
//! outstanding and no debt pays for one hedge of the largest item, and no more
//! than the cap is ever spent on top of the 5 % that completed bytes repay.
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

use std::cell::Cell;
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

/// The window an [`UplinkMeter`] reads over, in time the provider had work,
/// and how often [`run_live`] closes the windows that are due. A provider that
/// goes slow is read slower at once (the open window counts, see
/// [`UplinkMeter::read`]) and at its new rate a window later; an item picked
/// inside that gap can still land on it. A shorter window closes the gap and
/// reads noisier (one 16 KiB leaf is a whole window at 16 KB/s). A tuning
/// value, like [`GOODPUT_ALPHA`].
const UPLINK_WINDOW: Duration = Duration::from_secs(1);

/// How long an idle provider's uplink reading takes to recover to the best
/// known one. An old reading counts for less with time, linearly, so a provider
/// that was slow is asked again: one item at a time, because the item it then
/// holds stops the recovery. (A probe that ends inside one window, a refused
/// or failed one, closes none, so another may follow at once; the back-off
/// ladder bounds those as it always did.) With two fast providers at four streams each the first probe of a
/// dead-slow one comes after about a fifth of this, a minute; the busier the
/// others, the sooner. A tuning value: shorter finds a recovered provider
/// sooner and spends more probes on one that stays slow.
const UPLINK_RECOVERY: Duration = Duration::from_secs(300);

/// The least an uplink reading counts for in a cost, bytes/sec: a provider
/// read at zero keeps a finite cost.
const UPLINK_FLOOR: f64 = 1.0;

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
/// stalled, failed or was CANCELLED as the loser of a hedge race (so a provider
/// that served 90 % and then died, or served the front half of an item that a
/// hedge then finished, is not recorded as having served nothing). `elapsed` is
/// wall clock, dial included — one rule for every outcome;
/// `Stats.elapsed` (transfer only) is available at the [`transfer_once`] seam
/// when A4 wants a tighter goodput denominator.
///
/// A cancelled loser is credited bytes and time only ([`record_cancelled`]): it
/// is not a failure and it is not a sample of the provider's goodput.
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
    /// that became debt, i.e. bytes some peer sent that another peer had
    /// already sent. Both copies are in the per-provider `bytes`, so this is the
    /// part of [`total_bytes`](Self::total_bytes) that is not payload. This is the quantity [`HEDGE_BUDGET_RATIO`] is a budget
    /// FOR, and it is deliberately not "bytes a hedge delivered" — a hedge that
    /// wins delivers the half its primary never would have and duplicates
    /// nothing, so counting its delivery as a cost would libel the mechanism.
    pub hedge_bytes: u64,
}

impl AssignmentReport {
    /// Total payload bytes accounted to some provider. Counts what each
    /// provider MOVED, so a range a hedge race fetched on both sides is in it
    /// twice: the payload the run kept is this minus
    /// [`hedge_bytes`](Self::hedge_bytes), up to the bytes of an attempt that
    /// was dropped before they were imported.
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

/// What keeps hedging from turning a degraded network into a byte storm
/// (D4 §4.5): a cap that follows the items in flight, the charges of the hedges
/// in flight, and the duplicated bytes not yet repaid.
///
/// The cap is the larger of [`HEDGE_BUDGET_RATIO`] × the bytes in flight and
/// half of the largest item in flight — one hedge of it — and `0` when nothing
/// is. A hedge's charge stays `outstanding` until it settles, and what it
/// duplicated becomes `debt`, repaid at [`HEDGE_BUDGET_RATIO`] of the bytes of
/// each round that completes an item. Only `available = cap - outstanding - debt`
/// is ever spent; it is a function of those three alone, so a cap that falls and
/// rises again gives back nothing that is outstanding or owed. So: at every admission
/// `outstanding + debt <= cap`; with nothing outstanding and no debt one hedge
/// of the largest item is always payable; over a run the duplicated bytes stay
/// within 5 % of the completed bytes plus one cap; hedges that duplicate
/// nothing cost nothing. A hedge dropped unsettled (its item cancelled, the run
/// cut) is counted as duplicating nothing: what it and its primary both moved
/// is not measured there. The floor uses an item's ANNOUNCED size and a charge
/// the real one, so an item announced smaller than it is (or `0`, unknown) may
/// not be hedgeable.
#[derive(Debug)]
pub(crate) struct HedgeBudget {
    /// The sizes of the items in flight, with how many of each (a multiset:
    /// equal sizes retire one at a time).
    in_flight: BTreeMap<u64, usize>,
    /// The sum of `in_flight`, counting every item.
    in_flight_bytes: u64,
    /// The sum of the charges of the hedges admitted and not yet settled.
    outstanding: u64,
    /// Duplicated bytes not yet repaid by completed ones.
    debt: f64,
}

impl HedgeBudget {
    /// An empty live budget: no item in flight, so no cap.
    fn live() -> Self {
        Self {
            in_flight: BTreeMap::new(),
            in_flight_bytes: 0,
            outstanding: 0,
            debt: 0.0,
        }
    }

    /// What the items in flight call for.
    fn cap(&self) -> f64 {
        match self.in_flight.keys().next_back() {
            None => 0.0,
            Some(&largest) => {
                (HEDGE_BUDGET_RATIO * self.in_flight_bytes as f64).max(largest as f64 / 2.0)
            }
        }
    }

    /// What a hedge may still be charged: the cap less the charges outstanding
    /// and the debt, never below zero.
    fn available(&self) -> f64 {
        (self.cap() - self.outstanding as f64 - self.debt).max(0.0)
    }

    /// A live run's item joins the run.
    fn extend(&mut self, bytes: u64) {
        *self.in_flight.entry(bytes).or_insert(0) += 1;
        self.in_flight_bytes = self.in_flight_bytes.saturating_add(bytes);
    }

    /// A live run's item has ended.
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
    }

    /// Completed bytes repay duplicated ones at [`HEDGE_BUDGET_RATIO`].
    fn earn(&mut self, bytes: u64) {
        self.debt = (self.debt - HEDGE_BUDGET_RATIO * bytes as f64).max(0.0);
    }

    /// D4 §4.5: hedge only while at least half the cap is available AND this
    /// hedge's whole range fits. The half rule is the storm brake — it stops
    /// hedging long before the budget is actually exhausted, so a degraded run
    /// cannot ride it to zero one hedge at a time.
    ///
    /// The comparison is `>=`, not `>`: `available` lands on EXACTLY `cap / 2`
    /// in ordinary cases (a hedge of a quarter of the largest item outstanding
    /// against a floor cap, a debt repaid down to the half), and a strict `>`
    /// refused every hedge there for arithmetic reasons.
    fn try_charge(&mut self, bytes: u64) -> bool {
        let available = self.available();
        if available >= self.cap() / 2.0 && available >= bytes as f64 {
            self.outstanding = self.outstanding.saturating_add(bytes);
            true
        } else {
            false
        }
    }

    /// A hedge ends: its charge stops being outstanding and what it duplicated
    /// becomes debt. Settling a charge that is not outstanding is a bug, not a
    /// refill.
    fn settle(&mut self, charge: u64, duplicated: u64) {
        if charge > self.outstanding {
            debug_assert!(false, "a hedge was settled twice");
            tracing::error!(
                charge,
                outstanding = self.outstanding,
                "hedge budget settled a charge it never took"
            );
            self.outstanding = 0;
        } else {
            self.outstanding -= charge;
        }
        self.debt += duplicated.min(charge) as f64;
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
    /// What this provider delivers across ALL its streams: what
    /// [`pick_provider`] ranks by. Apart from `goodput` on purpose: that one is
    /// per stream, fed by completed transfers only, and the hedge trigger's
    /// deadline is built on it.
    uplink: UplinkMeter,
    stats: ProviderStats,
}

/// What one provider delivers to us, read off the byte counters of its
/// attempts: every byte of every attempt, completed, cancelled, stalled or
/// still running, over the time the provider had at least one attempt in
/// flight. Idle time is not counted, so the reading is the uplink and not how
/// much work the provider happened to get.
///
/// The reading is taken in windows of [`UPLINK_WINDOW`] of busy time. A window
/// that is due is closed by whoever looks at the meter next: the run's tick
/// ([`close_due_uplink_windows`]), a pick, or the end of the provider's last
/// attempt. A pick that finds the window not yet due reads it together with
/// the last closed one. Busy time includes a dial and a transfer that moves
/// nothing: a provider that accepts and sends no byte reads zero.
#[derive(Debug, Default)]
struct UplinkMeter {
    /// The byte counters of the attempts in flight, one per claim
    /// ([`claim_provider`] adds it, the claim's drop takes it out).
    attempts: Vec<Arc<AtomicU64>>,
    /// The bytes of the attempts that have ended.
    ended_bytes: u64,
    /// Time spent with an attempt in flight, up to `busy_since`.
    busy: Duration,
    /// When the current busy stretch began; `None` while idle.
    busy_since: Option<Instant>,
    /// The open window: the busy time and the bytes when it began.
    mark: Cell<(Duration, u64)>,
    /// Bytes per second of busy time over the last closed window, and when it
    /// closed. `None` until one has.
    last: Cell<Option<(f64, Instant)>>,
}

impl UplinkMeter {
    /// An attempt begins: its byte counter joins the meter.
    fn begin(&mut self, progress: Arc<AtomicU64>, now: Instant) {
        if self.attempts.is_empty() {
            self.busy_since = Some(now);
        }
        self.attempts.push(progress);
    }

    /// An attempt ends, however it ended: its bytes stay counted.
    fn end(&mut self, progress: &Arc<AtomicU64>, now: Instant) {
        if let Some(i) = self.attempts.iter().position(|p| Arc::ptr_eq(p, progress)) {
            let ended = self.attempts.swap_remove(i);
            self.ended_bytes += ended.load(Ordering::Relaxed);
        }
        if self.attempts.is_empty() {
            if let Some(since) = self.busy_since.take() {
                self.busy += now.saturating_duration_since(since);
            }
        }
        // A window that is due is closed here, so that it carries the time the
        // work ended and not the time of whichever pick looks next: the
        // recovery of an idle provider counts from it.
        let _ = self.read(now);
    }

    /// The reading at `now`, in bytes per second of busy time, and when the
    /// last window closed. `None` until a first window has.
    ///
    /// An open window that is due is closed first, and is then the reading.
    /// One that is not yet due is still evidence: the reading is over the last
    /// [`UPLINK_WINDOW`] of busy time, the open window's bytes plus the last
    /// closed reading for the part of that span the open window does not
    /// cover. So half a window of nothing already halves the reading, and a
    /// provider whose last attempts ended inside an open window (rescued by
    /// hedges, say) is not left with the reading from before they stopped
    /// moving. A hedge cancelled a moment after it began costs its provider a
    /// few percent, not its whole reading.
    fn read(&self, now: Instant) -> Option<(f64, Instant)> {
        let busy = self.busy
            + self
                .busy_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        let (busy_then, bytes_then) = self.mark.get();
        let window = busy.saturating_sub(busy_then);
        let bytes = self.ended_bytes
            + self
                .attempts
                .iter()
                .map(|p| p.load(Ordering::Relaxed))
                .sum::<u64>();
        let moved = bytes.saturating_sub(bytes_then) as f64;
        if window >= UPLINK_WINDOW {
            self.last.set(Some((moved / window.as_secs_f64(), now)));
            self.mark.set((busy, bytes));
            return self.last.get();
        }
        let (rate, closed_at) = self.last.get()?;
        let (open, full) = (window.as_secs_f64(), UPLINK_WINDOW.as_secs_f64());
        Some(((moved + rate * (full - open)) / full, closed_at))
    }
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

    /// The uplink this provider counts for at `now`, given its meter's
    /// `reading` ([`UplinkMeter::read`], taken once per pick). `top` is the
    /// best reading among the candidates of this pick, and what a provider
    /// never read is assumed to do. An idle provider's reading recovers linearly toward
    /// `top` over [`UPLINK_RECOVERY`]; one with an item in flight keeps its
    /// reading, so a probe is one item and not a stream of them.
    fn uplink_now(&self, reading: Option<(f64, Instant)>, top: f64, now: Instant) -> f64 {
        match reading {
            None => top,
            Some((rate, _)) if self.inflight > 0 || rate >= top => rate,
            Some((rate, read_at)) => {
                let age = now.saturating_duration_since(read_at);
                let recovered = (age.as_secs_f64() / UPLINK_RECOVERY.as_secs_f64()).min(1.0);
                rate + (top - rate) * recovered
            }
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
    /// The byte counter of the attempt this claim is for, on the provider's
    /// [`UplinkMeter`] for as long as the claim lives.
    progress: Arc<AtomicU64>,
}

impl InflightGuard {
    fn provider(&self) -> EndpointId {
        self.provider
    }

    /// The attempt's byte counter: hand it to [`transfer_once`].
    fn progress(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.progress)
    }

    /// The attempt is over though the claim is still held (a primary that
    /// failed while its hedge carries the round on): take its counter off the
    /// meter, so the provider is not read as busy and delivering nothing for
    /// as long as the hedge runs. The slot stays claimed until the drop.
    fn end_attempt(&self) {
        let mut guard = self
            .states
            .lock()
            .expect("assignment states mutex poisoned");
        if let Some(st) = guard.get_mut(&self.provider) {
            st.uplink.end(&self.progress, Instant::now());
        }
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
            st.uplink.end(&self.progress, Instant::now());
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
    let mut meter_tick = tokio::time::interval(UPLINK_WINDOW);
    meter_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
            _ = meter_tick.tick(), if !set.is_empty() => {
                close_due_uplink_windows(&states, Instant::now());
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
        stalls = report.stalls,
        hedges = report.hedges,
        hedge_bytes = report.hedge_bytes,
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
        // What the store held of the item when `missing` was read: a hedge
        // places its range in the primary's request by what the store has
        // gained since (`hedge_back_start`).
        let present_at_start = local.local_bytes();

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
        let primary_progress = claim.progress();
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
                    if let Some(mut h) = hedge.take() {
                        // The primary finished the WHOLE missing range, so every
                        // byte the hedge moved was fetched twice. Its own
                        // progress is already request-relative to the hedged
                        // range, so it needs no offsetting.
                        let hedge_moved = h.progress.load(Ordering::Relaxed);
                        let duplicated = hedge_moved.min(h.charge.bytes);
                        settle_hedge_loser(&mut h, duplicated);
                        // The cancelled hedge's own bytes and time, to its own
                        // provider (report only).
                        record_cancelled(&states, h.provider, hedge_moved, h.started.elapsed());
                        tracing::debug!(
                            child = index,
                            loser = %h.provider.fmt_short(),
                            refunded_bytes = h.charge.bytes - duplicated,
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
                    // Its transfer is gone; its claim lives to the round's end.
                    claim.end_attempt();
                }
                Step::Hedge(r) => {
                    let mut h = hedge.take().expect("the hedge branch only runs when armed");
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
                            let moved = primary_progress.load(Ordering::Relaxed);
                            // `moved` counts payload bytes inside the PRIMARY's
                            // request, which began at ROUND START; the hedged
                            // half begins `back_start` into that same request
                            // (see `HedgeRun::back_start`). What the primary
                            // moved PAST that point, and only that, was fetched
                            // twice.
                            let duplicated = primary_overlap(moved, h.back_start, h.charge.bytes);
                            settle_hedge_loser(&mut h, duplicated);
                            if completed_child {
                                // The hedge finished the child outright (ruling
                                // R3's "the last of the child" case above) — it
                                // is a completed assignment exactly like the
                                // unhedged success path below, so it feeds the
                                // same p95 window and repays debt the same way
                                // (after the settlement, as on that path).
                                note_completion(&ledger, hedge_elapsed, stats.payload_bytes_read);
                            }
                            if !primary_gone {
                                // Cancelling the primary IS dropping it; the
                                // `continue` below ends its scope. What it
                                // moved before that is credited to it (report
                                // only); a primary that already failed was
                                // credited by `note_fault`.
                                drop(primary);
                                record_cancelled(&states, provider, moved, started.elapsed());
                                tracing::debug!(
                                    child = index,
                                    loser = %provider.fmt_short(),
                                    refunded_bytes = h.charge.bytes - duplicated,
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
                            // With the primary still running, it fetches the
                            // hedge's bytes again: all of them were moved twice.
                            // With the primary gone, nothing does — only what
                            // the primary had already moved past the split was.
                            let moved = if primary_gone {
                                primary_overlap(
                                    primary_progress.load(Ordering::Relaxed),
                                    h.back_start,
                                    h.charge.bytes,
                                )
                                .min(fault.bytes())
                            } else {
                                fault.bytes()
                            }
                            .min(h.charge.bytes);
                            settle_hedge_loser(&mut h, moved);
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
                        present_at_start,
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
    /// (which began at ROUND START, before this hedge existed). Placing the cut
    /// in the primary's request also needs what the store gained in between:
    /// [`hedge_back_start`].
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
    /// The bytes charged to the budget when this hedge was armed — the size of
    /// the back range it asked for — and the duty to settle them.
    charge: HedgeCharge,
    /// How many bytes of the primary's OWN request come before the hedged
    /// range, so its progress counter can be turned into "how much of the
    /// charged range did it duplicate" ([`primary_overlap`]). Request-relative,
    /// never a blob offset; placed by [`hedge_back_start`] when the hedge is
    /// armed.
    back_start: u64,
    started: Instant,
    /// Keeps the hedge provider's assignment slot claimed for as long as the
    /// hedge runs; released structurally when this struct drops.
    _claim: InflightGuard,
}

/// A hedge's charge, settled exactly once: by [`HedgeCharge::settle`] when the
/// hedge ends in a race, or by `Drop` with nothing duplicated when it is dropped
/// unsettled (its item was cancelled, the run was cut, the future was dropped).
///
/// The ledger mutex is only ever taken for a moment and no future is dropped
/// while it is held, so `Drop` cannot run under the lock it takes.
struct HedgeCharge {
    ledger: Ledger,
    bytes: u64,
    settled: bool,
}

impl HedgeCharge {
    /// Settle with `duplicated` bytes of the range moved twice (at most the
    /// charge); a second call does nothing.
    fn settle(&mut self, duplicated: u64) {
        if std::mem::replace(&mut self.settled, true) {
            debug_assert!(false, "a hedge charge was settled twice");
            tracing::error!(bytes = self.bytes, "hedge charge settled twice");
            return;
        }
        let duplicated = duplicated.min(self.bytes);
        let mut l = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        l.budget.settle(self.bytes, duplicated);
        // What was moved twice is what hedging actually cost.
        l.hedge_bytes = l.hedge_bytes.saturating_add(duplicated);
    }
}

impl Drop for HedgeCharge {
    fn drop(&mut self) {
        if !self.settled {
            self.settled = true;
            // Poisoning is tolerated: this may run while a panic unwinds.
            let mut l = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            l.budget.settle(self.bytes, 0);
        }
    }
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

/// A completed assignment: repay hedge debt and widen the p95 sample.
fn note_completion(ledger: &Ledger, elapsed: Duration, bytes: u64) {
    let mut l = ledger.lock().expect("hedge ledger mutex poisoned");
    l.completions.record(elapsed);
    l.budget.earn(bytes);
}

/// Settle a hedge that ran to an end: its charge stops being outstanding and
/// the part of it the loser actually moved becomes debt.
///
/// `duplicated` is how much of the CHARGED range was moved twice — the hedge's
/// own progress when the primary won or the hedge failed beside a live primary
/// (the primary fetches those bytes again), or the primary's progress past the
/// split point when the hedge won or failed after the primary was gone. A hedge that wins outright against
/// a primary still below the split therefore costs nothing, which is the honest
/// answer: nothing was fetched twice. A hedge that ends any other way is
/// settled by its [`HedgeCharge`] with nothing duplicated.
fn settle_hedge_loser(hedge: &mut HedgeRun, duplicated: u64) {
    hedge.charge.settle(duplicated);
}

/// Where a hedge's range begins in its primary's request, in bytes
/// ([`HedgeRun::back_start`]).
///
/// The primary was handed what was missing when its round began, with
/// `present_at_start` bytes of the item already local, and moves it in order.
/// What the store has gained since (`present_at_arm - present_at_start`) is the
/// primary's prefix as the store kept it, and `split_rel` more missing bytes lie
/// in front of the cut. Never the primary's progress counter in place of the
/// first term: the counter counts a leaf as soon as it is decoded, while the
/// store writes leaves in batches that the next parent closes, so the counter
/// runs one or two 16 KiB groups ahead of the bitfield the split is cut from.
/// Placed by the counter, the cut lands that far down the primary's request,
/// and the primary's first group or two of the hedged range counted for nobody.
///
/// Exact while the store gains this item from the primary alone; an earlier
/// hedge of the same round that failed after storing bytes past the cut moves
/// it by those bytes (its own settlement already counted them).
fn hedge_back_start(present_at_start: u64, present_at_arm: u64, split_rel: u64) -> u64 {
    present_at_arm
        .saturating_sub(present_at_start)
        .saturating_add(split_rel)
}

/// Bytes of a hedge's charged range its primary moved too, the primary being
/// `primary_moved` bytes into its own request and the hedged range beginning
/// `back_start` bytes into it; never more than the charge.
fn primary_overlap(primary_moved: u64, back_start: u64, charge: u64) -> u64 {
    primary_moved.saturating_sub(back_start).min(charge)
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
    present_at_start: u64,
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
    // is for the TAIL. A uniformly slow peer that keeps moving is never cut by
    // the stall ceiling (it fires only on no progress at all), so its item
    // simply crawls at that peer's rate: that is A4's ranking problem.
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
    // From here the charge is outstanding, and this guard owes its settlement
    // however the hedge ends.
    let charge_guard = HedgeCharge {
        ledger: Arc::clone(ledger),
        bytes: charge,
        settled: false,
    };

    let hedge_provider = claim.provider();
    let progress = claim.progress();
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
    // Placed against the SAME snapshot the split was cut from: `split_rel` is
    // an offset into this bitfield's missing set.
    let back_start = hedge_back_start(present_at_start, bitfield.total_bytes(), split.split_rel);
    Some(HedgeRun {
        provider: hedge_provider,
        fut,
        progress,
        charge: charge_guard,
        back_start,
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
    let progress = Arc::new(AtomicU64::new(0));
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
        let st = guard.entry(chosen).or_default();
        st.inflight += 1;
        st.uplink.begin(Arc::clone(&progress), now);
        chosen
    };
    Some(InflightGuard {
        states: Arc::clone(states),
        provider: chosen,
        progress,
    })
}

/// The provider an item should go to: among the providers not in backoff (nor
/// busy), the one it should finish on first. A provider's cost is
/// `(in flight + 1) / uplink`: the streams it would then carry over what it
/// delivers across all of them ([`ProviderState::uplink_now`]). So a fast
/// provider with work in hand still beats an idle slow one, and stops beating
/// it once enough is piled on it. Ties go to the fewest failures, then to list
/// order.
///
/// A provider never read (a live one that just appeared, or any provider in
/// the first [`UPLINK_WINDOW`] of a run) counts as the best one read: unknown
/// is assumed good, and its first window corrects that. With nobody read every
/// cost is `in flight + 1`, the least-loaded pick.
///
/// Alone in a function: the spec's `RankedProviders` (a persistent table, RTT)
/// would replace this body and nothing else. D4 §9: ranking may only influence
/// ORDER, never a static partition, so the loop around it stays work-stealing
/// whatever this returns.
fn pick_provider(
    states: &HashMap<EndpointId, ProviderState>,
    providers: &[EndpointId],
    now: Instant,
) -> Option<EndpointId> {
    // Each meter is read ONCE: a second read a moment later would see counters
    // that have moved, and could break a near-tie between two providers.
    struct Candidate<'a> {
        order: usize,
        id: EndpointId,
        state: Option<&'a ProviderState>,
        reading: Option<(f64, Instant)>,
    }
    let candidates: Vec<Candidate<'_>> = providers
        .iter()
        .enumerate()
        .filter_map(|(order, id)| match states.get(id) {
            Some(st) if !st.available(now) => None,
            // A provider with no state yet (a live one that just appeared) is
            // idle and unread.
            state => Some(Candidate {
                order,
                id: *id,
                state,
                reading: state.and_then(|st| st.uplink.read(now)),
            }),
        })
        .collect();
    let top = candidates
        .iter()
        .filter_map(|c| c.reading)
        .map(|(rate, _)| rate)
        .reduce(f64::max);
    candidates
        .into_iter()
        .map(|c| {
            let (inflight, failures) = c.state.map_or((0, 0), |st| (st.inflight, st.failures));
            let uplink = match top {
                None => 1.0,
                Some(top) => c
                    .state
                    .map_or(top, |st| st.uplink_now(c.reading, top, now))
                    .max(UPLINK_FLOOR),
            };
            (f64::from(inflight + 1) / uplink, failures, c.order, c.id)
        })
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)))
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

/// Credit the loser of a hedge race — the attempt that was cancelled because
/// the other side finished — with what it moved and how long it ran.
///
/// Report-only. A cancelled attempt is neither a success nor a failure, so
/// nothing a pick or a hedge decision reads moves: not the consecutive-failure
/// count, the back-off, the goodput average, the stall count or the child count.
/// Without this the front half a primary served before a hedge finished the item
/// is real payload the run kept, credited to nobody.
fn record_cancelled(states: &States, provider: EndpointId, bytes: u64, elapsed: Duration) {
    credit_attempt(states, provider, bytes, elapsed);
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

/// Close every provider's uplink window that is due. [`run_live`] calls it
/// every [`UPLINK_WINDOW`] while anything is in flight: a pick reads a meter
/// over its last window, and with large items picks are many seconds apart, so
/// without this a window would be as long as the gap between two picks and a
/// provider that went slow just before the second would read almost as before.
fn close_due_uplink_windows(states: &States, now: Instant) {
    let guard = states.lock().expect("assignment states mutex poisoned");
    for st in guard.values() {
        let _ = st.uplink.read(now);
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

    /// A cancelled hedge loser is credited bytes and time and nothing else:
    /// nothing a provider pick or a hedge decision reads moves.
    #[test]
    fn a_cancelled_loser_is_credited_bytes_and_time_only() {
        let a = distinct_id();
        let states: States = Arc::new(Mutex::new(states_of(vec![(a, ProviderState::default())])));
        // A provider with a failure on record and a goodput reading.
        record_failure(&states, a, &TransferFault::Stalled { bytes: 5 }, Duration::from_millis(2));
        record_success(&states, a, &Stats::default(), Duration::from_millis(1), false);
        record_failure(&states, a, &TransferFault::Stalled { bytes: 5 }, Duration::from_millis(2));
        let (failures, next_try, goodput, samples, stalls, children, stat_failures) = {
            let g = states.lock().unwrap();
            let st = &g[&a];
            (
                st.failures,
                st.next_try,
                st.goodput,
                st.goodput_samples,
                st.stalls,
                st.stats.children,
                st.stats.failures,
            )
        };
        let (bytes0, elapsed0) = {
            let g = states.lock().unwrap();
            (g[&a].stats.bytes, g[&a].stats.elapsed)
        };

        record_cancelled(&states, a, 1234, Duration::from_millis(40));

        let g = states.lock().unwrap();
        let st = &g[&a];
        assert_eq!(st.stats.bytes, bytes0 + 1234);
        assert_eq!(st.stats.elapsed, elapsed0 + Duration::from_millis(40));
        assert_eq!(st.failures, failures);
        assert_eq!(st.stats.failures, stat_failures);
        assert_eq!(st.next_try, next_try);
        assert_eq!(st.goodput, goodput);
        assert_eq!(st.goodput_samples, samples);
        assert_eq!(st.stalls, stalls);
        assert_eq!(st.stats.children, children);
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// A live budget with `n` items of `size` bytes in flight.
    fn budget_with(n: usize, size: u64) -> HedgeBudget {
        let mut b = HedgeBudget::live();
        for _ in 0..n {
            b.extend(size);
        }
        b
    }

    /// A ledger around `budget`, for the charge guard.
    fn ledger_of(budget: HedgeBudget) -> Ledger {
        Arc::new(Mutex::new(HedgeLedger {
            budget,
            completions: CompletionWindow::default(),
            hedges: 0,
            hedge_bytes: 0,
        }))
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

    /// A hedge that wins is charged what its primary moved past the cut, and the
    /// cut is placed in the primary's request by what the store holds: the
    /// primary's progress counter runs a group or two ahead of the store, and
    /// placing the cut by it counted nothing in the first case below.
    #[test]
    fn a_won_hedge_counts_what_the_primary_moved_past_the_cut() {
        const GROUP: u64 = 16 * CHUNK_BYTES;
        const SIZE: u64 = 256 * CHUNK_BYTES;
        let missing_from = |chunk: u64| ChunkRanges::chunks(chunk..256);

        // The cut on a group boundary: the store holds 14 groups (the
        // primary's counter reads 15), the last group is hedged, and the
        // primary finishes it too, so all of it was fetched twice.
        let s = split_missing_at_midpoint(&missing_from(224), SIZE).expect("splittable");
        assert_eq!((s.split_rel, s.charge), (GROUP, GROUP));
        let back_start = hedge_back_start(0, 14 * GROUP, s.split_rel);
        assert_eq!(
            back_start,
            240 * CHUNK_BYTES,
            "the hedged range begins at chunk 240"
        );
        assert_eq!(primary_overlap(SIZE, back_start, s.charge), GROUP);
        assert_eq!(primary_overlap(back_start, back_start, s.charge), 0);

        // The cut inside a group: 11 groups held, 80 chunks missing, the cut at
        // chunk 216, half way through group 13.
        let s = split_missing_at_midpoint(&missing_from(176), SIZE).expect("splittable");
        assert_eq!(
            (s.split_rel, s.charge),
            (40 * CHUNK_BYTES, 40 * CHUNK_BYTES)
        );
        let back_start = hedge_back_start(0, 11 * GROUP, s.split_rel);
        assert_eq!(back_start, 216 * CHUNK_BYTES);
        assert_eq!(
            primary_overlap(13 * GROUP, back_start, s.charge),
            0,
            "a primary short of the cut duplicated nothing"
        );
        assert_eq!(
            primary_overlap(14 * GROUP, back_start, s.charge),
            8 * CHUNK_BYTES,
            "a primary that finished group 13 moved its 8 chunks past the cut"
        );

        // A hedged range smaller than a group: one group missing, cut in half.
        let s = split_missing_at_midpoint(&missing_from(240), SIZE).expect("splittable");
        assert_eq!(s.charge, 8 * CHUNK_BYTES);
        let back_start = hedge_back_start(0, 15 * GROUP, s.split_rel);
        assert_eq!(
            primary_overlap(back_start + 4 * CHUNK_BYTES, back_start, s.charge),
            4 * CHUNK_BYTES,
            "less than a group past the cut counts what it moved"
        );
        assert_eq!(primary_overlap(SIZE, back_start, s.charge), s.charge);

        // A round that began part way: [144,192) missing, the rest local; the
        // store has gained one group of it when [160,192) is cut at chunk 176,
        // 32 chunks into the primary's request.
        let present_at_start = SIZE - 48 * CHUNK_BYTES;
        let s =
            split_missing_at_midpoint(&ChunkRanges::chunks(160u64..192), SIZE).expect("splittable");
        let back_start = hedge_back_start(present_at_start, present_at_start + GROUP, s.split_rel);
        assert_eq!(back_start, 2 * GROUP);
        assert_eq!(primary_overlap(3 * GROUP, back_start, s.charge), GROUP);

        // Never more than the charge, whatever the counter reads.
        assert_eq!(primary_overlap(u64::MAX, back_start, s.charge), s.charge);
    }

    /// A hedge is admitted at EXACTLY half the cap (ruling R1): `available`
    /// lands there when a hedge of a quarter of the largest item is outstanding
    /// against a floor cap, and a strict `>` refused every hedge there.
    #[test]
    fn hedge_budget_admits_at_exactly_half_the_cap() {
        let mut b = budget_with(1, 2_000_000);
        assert!(close(b.cap(), 1_000_000.0));
        assert!(b.try_charge(500_000));
        assert!(close(b.available(), b.cap() / 2.0), "{}", b.available());
        assert!(
            b.try_charge(1),
            "available sitting exactly on cap/2 must still admit a hedge"
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

    /// A provider with `inflight` items whose meter last read `rate` at `at`.
    fn read(inflight: u32, rate: f64, at: Instant) -> ProviderState {
        ProviderState {
            inflight,
            uplink: UplinkMeter {
                last: Cell::new(Some((rate, at))),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The pick is where the item should finish first: streams over uplink. A
    /// fast provider with work in hand beats an idle slow one, until enough is
    /// piled on it.
    #[test]
    fn pick_provider_sends_an_item_where_it_finishes_first() {
        let (fast, slow) = (distinct_id(), distinct_id());
        let now = Instant::now();
        // 4 streams on 400 KB/s is 100 KB/s for the item; the idle one gives 20.
        let st = states_of(vec![
            (fast, read(3, 400_000.0, now)),
            (slow, read(0, 20_000.0, now)),
        ]);
        assert_eq!(pick_provider(&st, &[slow, fast], now), Some(fast));
        // 20 streams on it is 20 KB/s each, 21 is less: the slow one wins.
        let st = states_of(vec![
            (fast, read(20, 400_000.0, now)),
            (slow, read(0, 20_000.0, now)),
        ]);
        assert_eq!(pick_provider(&st, &[fast, slow], now), Some(slow));
        // Equal uplinks: the least loaded, as before.
        let st = states_of(vec![
            (fast, read(2, 400_000.0, now)),
            (slow, read(1, 400_000.0, now)),
        ]);
        assert_eq!(pick_provider(&st, &[fast, slow], now), Some(slow));
    }

    /// A provider never read counts as the best one read, so it is tried; and
    /// a provider read at zero keeps a finite cost.
    #[test]
    fn pick_provider_assumes_an_unread_provider_is_as_good_as_the_best() {
        let (fast, slow, new, dead) = (distinct_id(), distinct_id(), distinct_id(), distinct_id());
        let now = Instant::now();
        let st = states_of(vec![
            (fast, read(1, 400_000.0, now)),
            (slow, read(0, 20_000.0, now)),
        ]);
        // Idle and assumed 400 KB/s: ahead of the fast one's second stream.
        assert_eq!(pick_provider(&st, &[fast, slow, new], now), Some(new));
        // With one item on it, it is level with the fast one: list order.
        let st = states_of(vec![
            (fast, read(1, 400_000.0, now)),
            (
                new,
                ProviderState {
                    inflight: 1,
                    ..Default::default()
                },
            ),
        ]);
        assert_eq!(pick_provider(&st, &[fast, new], now), Some(fast));
        assert_eq!(pick_provider(&st, &[new, fast], now), Some(new));
        // Zero is a reading too, and the only candidate is still picked.
        let st = states_of(vec![(dead, read(0, 0.0, now))]);
        assert_eq!(pick_provider(&st, &[dead], now), Some(dead));
        let st = states_of(vec![
            (dead, read(0, 0.0, now)),
            (fast, read(7, 400_000.0, now)),
        ]);
        assert_eq!(pick_provider(&st, &[dead, fast], now), Some(fast));
    }

    /// An idle provider's old reading recovers toward the best one, so a slow
    /// provider is asked again; the item it then holds stops the recovery, so
    /// the probe is one item.
    #[test]
    fn an_idle_slow_provider_recovers_and_gets_one_probe() {
        let (fast, slow) = (distinct_id(), distinct_id());
        let read_at = Instant::now();
        let at = |secs: u64| read_at + Duration::from_secs(secs);
        let states = |slow_inflight: u32| {
            states_of(vec![
                (fast, read(4, 400_000.0, read_at)),
                (slow, read(slow_inflight, 1_000.0, read_at)),
            ])
        };
        // The fast one gives the item 80 KB/s. The slow one's reading passes
        // that a fifth of the way to 400 KB/s: after 60 of the 300 seconds.
        assert_eq!(pick_provider(&states(0), &[slow, fast], at(0)), Some(fast));
        assert_eq!(pick_provider(&states(0), &[slow, fast], at(55)), Some(fast));
        assert_eq!(pick_provider(&states(0), &[slow, fast], at(65)), Some(slow));
        // With the probe in flight the reading is the measured one again.
        assert_eq!(pick_provider(&states(1), &[slow, fast], at(65)), Some(fast));
        assert_eq!(pick_provider(&states(1), &[slow, fast], at(3_000)), Some(fast));
        // Fully recovered and both idle: level, list order.
        let idle = states_of(vec![
            (fast, read(0, 400_000.0, read_at)),
            (slow, read(0, 1_000.0, read_at)),
        ]);
        assert_eq!(pick_provider(&idle, &[slow, fast], at(299)), Some(fast));
        assert_eq!(pick_provider(&idle, &[slow, fast], at(300)), Some(slow));
    }

    /// The meter reads every byte of every attempt over the time the provider
    /// had work: all its streams together, ended attempts included, idle time
    /// left out, one window at a time.
    #[test]
    fn the_uplink_meter_reads_bytes_over_busy_time_in_windows() {
        let began = Instant::now();
        let at = |ms: u64| began + Duration::from_millis(ms);
        let counter = |bytes: u64| Arc::new(AtomicU64::new(bytes));
        let mut meter = UplinkMeter::default();
        assert_eq!(meter.read(at(5_000)), None, "an idle meter never closes a window");

        // Two streams from t = 0.
        let (first, second) = (counter(0), counter(0));
        meter.begin(Arc::clone(&first), at(0));
        meter.begin(Arc::clone(&second), at(0));
        first.store(30_000, Ordering::Relaxed);
        second.store(50_000, Ordering::Relaxed);
        assert_eq!(meter.read(at(900)), None, "the first window is still open");
        // 80 000 bytes in the 2 s the window grew to: both streams together.
        assert_eq!(meter.read(at(2_000)), Some((40_000.0, at(2_000))));
        // Half a window later nothing more has moved: half of the last
        // second was at 40 KB/s, half at nothing.
        assert_eq!(meter.read(at(2_500)), Some((20_000.0, at(2_000))));
        // One stream ends; its bytes stay counted. The other stops moving.
        first.store(130_000, Ordering::Relaxed);
        meter.end(&first, at(2_500));
        assert_eq!(meter.read(at(3_000)), Some((100_000.0, at(3_000))));
        // The next window: nothing moved, whatever came before.
        assert_eq!(meter.read(at(4_000)), Some((0.0, at(4_000))));

        // Idle from 4.5 s to 60 s: no busy time passes and no window closes.
        meter.end(&second, at(4_500));
        assert_eq!(meter.read(at(60_000)), Some((0.0, at(4_000))));
        // Work again: the open window had 0.5 s of busy time in it, and needs
        // 0.5 s more. 25 000 bytes over that second.
        let third = counter(0);
        meter.begin(Arc::clone(&third), at(60_000));
        third.store(25_000, Ordering::Relaxed);
        assert_eq!(meter.read(at(60_250)), Some((25_000.0, at(4_000))));
        assert_eq!(meter.read(at(60_500)), Some((25_000.0, at(60_500))));
        // A provider that goes idle inside an open window is read through it:
        // the half second of nothing counts against the 25 KB/s before it.
        meter.end(&third, at(61_000));
        assert_eq!(meter.read(at(90_000)), Some((12_500.0, at(60_500))));

        // The end of the last attempt closes a window that is due, at the time
        // the work ended: 30 000 bytes over the 2 s of busy time since the
        // last close (the idle half second before, and these 1.5 s).
        let fourth = counter(0);
        meter.begin(Arc::clone(&fourth), at(100_000));
        fourth.store(30_000, Ordering::Relaxed);
        meter.end(&fourth, at(101_500));
        assert_eq!(meter.read(at(200_000)), Some((15_000.0, at(101_500))));
    }

    /// The run's tick closes the windows that are due, whoever picks next and
    /// whenever: a provider that stopped is read at zero a window later, not
    /// averaged over the whole gap between two picks.
    #[test]
    fn the_run_tick_closes_due_windows_between_picks() {
        let (a, b) = (distinct_id(), distinct_id());
        let began = Instant::now();
        let at = |ms: u64| began + Duration::from_millis(ms);
        let (moving, stopped) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let mut on_a = UplinkMeter::default();
        on_a.begin(Arc::clone(&moving), began);
        let mut on_b = UplinkMeter::default();
        on_b.begin(Arc::clone(&stopped), began);
        let provider = |inflight, uplink| ProviderState {
            inflight,
            uplink,
            ..Default::default()
        };
        let states: States = Arc::new(Mutex::new(states_of(vec![
            (a, provider(7, on_a)),
            (b, provider(1, on_b)),
        ])));
        let last = |id: EndpointId| states.lock().unwrap().get(&id).unwrap().uplink.last.get();

        // Both move 500 000 bytes in five seconds; then `b` stops.
        moving.store(500_000, Ordering::Relaxed);
        stopped.store(500_000, Ordering::Relaxed);
        close_due_uplink_windows(&states, at(5_000));
        assert_eq!(last(a), Some((100_000.0, at(5_000))));
        assert_eq!(last(b), Some((100_000.0, at(5_000))));
        moving.store(600_000, Ordering::Relaxed);
        close_due_uplink_windows(&states, at(5_500));
        assert_eq!(last(b), Some((100_000.0, at(5_000))), "not due yet");
        close_due_uplink_windows(&states, at(6_000));
        assert_eq!(last(a), Some((100_000.0, at(6_000))));
        assert_eq!(last(b), Some((0.0, at(6_000))));
        // A pick ten seconds later, `a` still at 100 KB/s with seven streams:
        // eight on that is 12.5 KB/s for the item, and the stopped one gives
        // nothing. Had no window been closed since the start, `b` would read
        // 500 000 bytes over 16 s, 31 KB/s, 15.6 for the item, and win.
        moving.store(1_600_000, Ordering::Relaxed);
        let guard = states.lock().unwrap();
        assert_eq!(pick_provider(&guard, &[b, a], at(16_000)), Some(a));
    }

    /// A primary that failed while its hedge runs on keeps its claim to the
    /// round's end, but its counter leaves the meter at once: the provider is
    /// not read as busy with nothing moving, and the drop counts nothing twice.
    #[test]
    fn an_attempt_ended_early_leaves_the_meter_and_keeps_the_slot() {
        let a = distinct_id();
        let states: States = Arc::new(Mutex::new(HashMap::new()));
        let claim = claim_provider(&states, &[a], None).expect("an idle provider is claimed");
        claim.progress().store(9, Ordering::Relaxed);
        claim.end_attempt();
        {
            let guard = states.lock().unwrap();
            let st = guard.get(&a).unwrap();
            assert_eq!((st.inflight, st.uplink.attempts.len()), (1, 0));
            assert_eq!(st.uplink.ended_bytes, 9);
            assert!(st.uplink.busy_since.is_none());
        }
        drop(claim);
        let guard = states.lock().unwrap();
        let st = guard.get(&a).unwrap();
        assert_eq!((st.inflight, st.uplink.attempts.len()), (0, 0));
        assert_eq!(st.uplink.ended_bytes, 9);
    }

    /// A provider whose streams have stopped moving loses the pick within a
    /// window, before any of its attempts ends, whether it was read before or
    /// not.
    #[test]
    fn a_provider_whose_streams_stopped_gets_no_new_item() {
        let (fast, went_slow) = (distinct_id(), distinct_id());
        let began = Instant::now();
        let at = |ms: u64| began + Duration::from_millis(ms);
        // Two streams in flight since t = 0, 5 000 bytes each so far.
        let states = |read_before: Option<(f64, Instant)>| {
            let mut meter = UplinkMeter {
                last: Cell::new(read_before),
                ..Default::default()
            };
            meter.begin(Arc::new(AtomicU64::new(5_000)), began);
            meter.begin(Arc::new(AtomicU64::new(5_000)), began);
            states_of(vec![
                (fast, read(3, 400_000.0, began)),
                (
                    went_slow,
                    ProviderState {
                        inflight: 2,
                        uplink: meter,
                        ..Default::default()
                    },
                ),
            ])
        };
        // Never read: inside its first window it counts as 400 KB/s, and with
        // two streams against three it is the cheaper one. Then the window
        // closes at 10 KB/s: three streams on that against four on 400 KB/s.
        let st = states(None);
        assert_eq!(pick_provider(&st, &[fast, went_slow], at(900)), Some(went_slow));
        assert_eq!(pick_provider(&st, &[fast, went_slow], at(1_000)), Some(fast));
        assert_eq!(pick_provider(&st, &[fast, went_slow], at(1_500)), Some(fast));
        // Read at 400 KB/s before: a quarter of a window in, with 10 KB of the
        // 100 KB that pace would have moved, the reading is 310 KB/s. Three
        // streams on that are still ahead of four on 400. Half a window in it
        // is 210 KB/s, and they are not.
        let st = states(Some((400_000.0, began)));
        assert_eq!(pick_provider(&st, &[fast, went_slow], at(250)), Some(went_slow));
        assert_eq!(pick_provider(&st, &[fast, went_slow], at(500)), Some(fast));
        assert_eq!(pick_provider(&st, &[fast, went_slow], at(1_000)), Some(fast));
    }

    /// A claim puts its attempt on the provider's meter and its drop takes it
    /// out, keeping the bytes.
    #[test]
    fn a_claim_meters_its_attempt_until_it_is_dropped() {
        let a = distinct_id();
        let states: States = Arc::new(Mutex::new(HashMap::new()));
        let first = claim_provider(&states, &[a], None).expect("an idle provider is claimed");
        let second = claim_provider(&states, &[a], None).expect("and claimed again");
        first.progress().store(7, Ordering::Relaxed);
        {
            let guard = states.lock().unwrap();
            let st = guard.get(&a).unwrap();
            assert_eq!((st.inflight, st.uplink.attempts.len()), (2, 2));
            assert!(st.uplink.busy_since.is_some());
        }
        drop(first);
        {
            let guard = states.lock().unwrap();
            let st = guard.get(&a).unwrap();
            assert_eq!((st.inflight, st.uplink.attempts.len()), (1, 1));
            assert!(Arc::ptr_eq(&st.uplink.attempts[0], &second.progress));
            assert_eq!(st.uplink.ended_bytes, 7);
            assert!(st.uplink.busy_since.is_some(), "one attempt is still in flight");
        }
        drop(second);
        let guard = states.lock().unwrap();
        let st = guard.get(&a).unwrap();
        assert_eq!((st.inflight, st.uplink.attempts.len()), (0, 0));
        assert!(st.uplink.busy_since.is_none());
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

    /// A live run's cap follows the items in flight — the larger of 5 % of their
    /// bytes and half the largest — growing with each item taken and shrinking
    /// as each ends, and `available` never exceeds it.
    #[test]
    fn a_live_budget_follows_the_items_in_flight() {
        let mut b = HedgeBudget::live();
        assert!(close(b.cap(), 0.0) && close(b.available(), 0.0), "empty at the start");
        b.extend(1_000_000);
        assert!(close(b.cap(), 500_000.0), "one hedge of the item: {}", b.cap());
        assert!(close(b.available(), 500_000.0), "an item brings its share");
        b.extend(3_000_000);
        // 5 % of 4 MB is 200 kB; half of the 3 MB item is 1.5 MB.
        assert!(close(b.cap(), 1_500_000.0), "{}", b.cap());
        assert!(close(b.available(), 1_500_000.0), "{}", b.available());
        assert!(b.try_charge(60_000));
        b.retire(3_000_000);
        assert!(close(b.cap(), 500_000.0), "the floor drops to the 1 MB item: {}", b.cap());
        assert!(b.available() <= b.cap(), "available follows the cap down");
        b.retire(1_000_000);
        assert!(close(b.cap(), 0.0), "nothing in flight, no budget: {}", b.cap());
        assert!(close(b.available(), 0.0));
    }

    /// One item in flight: one hedge of half that item is admitted, a second is
    /// refused while the first is outstanding, and the settlement of the first
    /// (nothing duplicated) pays for it again.
    #[test]
    fn a_live_budget_admits_one_hedge_of_a_lone_item() {
        let mut b = budget_with(1, 1_000_000);
        assert!(b.try_charge(500_000), "half the item, from an idle budget");
        assert!(!b.try_charge(500_000), "a second one is refused while the first is out");
        b.settle(500_000, 0);
        assert!(b.try_charge(500_000), "the settlement pays for it again");
        b.settle(500_000, 0);
        assert!(!b.try_charge(500_001), "more than half the item is never payable");
    }

    /// `settle` turns what was duplicated into debt and takes the charge off the
    /// outstanding sum; completed bytes repay the debt at 5 %, never below zero.
    #[test]
    fn a_settlement_leaves_debt_that_completed_bytes_repay() {
        let mut b = budget_with(1, 2_000_000);
        assert!(b.try_charge(400_000));
        assert_eq!(b.outstanding, 400_000);
        assert!(close(b.available(), 600_000.0));
        b.settle(400_000, 150_000);
        assert_eq!(b.outstanding, 0);
        assert!(close(b.debt, 150_000.0), "{}", b.debt);
        assert!(close(b.available(), 850_000.0), "debt reduces what is available");
        // 5 % of 1 MB repays 50 kB.
        b.earn(1_000_000);
        assert!(close(b.debt, 100_000.0), "{}", b.debt);
        b.earn(u64::MAX / 2);
        assert!(close(b.debt, 0.0), "repaying never goes below zero: {}", b.debt);
        assert!(close(b.available(), 1_000_000.0));
        // A loser cannot have duplicated more than its charge.
        assert!(b.try_charge(10_000));
        b.settle(10_000, 99_999);
        assert!(close(b.debt, 10_000.0), "{}", b.debt);
    }

    /// The half rule: with a hedge outstanding that takes `available` below
    /// half the cap nothing is admitted, however small; its settlement reopens
    /// hedging.
    #[test]
    fn a_live_budget_refuses_below_half_the_cap_and_a_hedge_larger_than_available() {
        // 100 items of 100 kB: cap = 5 % of 10 MB = 500 kB, the floor is 50 kB.
        let mut b = budget_with(100, 100_000);
        assert!(close(b.cap(), 500_000.0));
        assert!(b.try_charge(200_000));
        assert!(b.try_charge(60_000), "300 kB available is above half the cap");
        assert!(close(b.available(), 240_000.0));
        assert!(
            !b.try_charge(1),
            "below half the cap the budget refuses even a one-byte hedge — that is \
             the storm brake, not the exhaustion check"
        );
        b.settle(60_000, 0);
        assert!(b.try_charge(1), "a settlement reopens the gate");
        b.settle(1, 0);
        assert!(!b.try_charge(300_001), "a hedge larger than what is available is refused");
    }

    /// The review's sequence: the cap falls when the large item completes and
    /// rises when the next one joins, while a hedge on the small item is still
    /// out. The cap moving must not refill the budget: the hedge's charge is
    /// still outstanding. (Token arithmetic gave 50 available here and admitted
    /// a second hedge of 50.)
    #[test]
    fn the_cap_falling_and_rising_does_not_refill_the_budget() {
        const MB: u64 = 1_000_000;
        let mut b = HedgeBudget::live();
        b.extend(100 * MB);
        assert!(close(b.cap(), 50.0 * MB as f64));
        b.extend(20 * MB);
        assert!(close(b.cap(), 50.0 * MB as f64), "the floor still rules: {}", b.cap());
        assert!(b.try_charge(10 * MB), "a hedge on the 20 MB item");
        // The 100 MB item completes: it earns, then retires.
        b.earn(100 * MB);
        b.retire(100 * MB);
        assert!(close(b.cap(), 10.0 * MB as f64), "{}", b.cap());
        assert!(close(b.available(), 0.0), "{}", b.available());
        b.extend(100 * MB);
        assert!(close(b.cap(), 50.0 * MB as f64), "{}", b.cap());
        assert!(
            close(b.available(), b.cap() - (10 * MB) as f64),
            "the 10 MB still outstanding is not given back: {}",
            b.available()
        );
        assert!(!b.try_charge(50 * MB), "a hedge of half the new item is refused");
        assert!(b.try_charge(40 * MB), "what is really left still pays");
    }

    /// One receive stream, frames one at a time: every frame's hedge duplicates
    /// its whole charge. A frame that completes does not hand the next one a
    /// fresh budget; the duplicated bytes admitted stay within 5 % of the
    /// completed bytes plus one cap, and hedging does come back as the debt is
    /// repaid.
    #[test]
    fn one_stream_frames_one_at_a_time_cannot_outspend_the_ratio() {
        const FRAME: u64 = 100_000;
        let mut b = HedgeBudget::live();
        let (mut completed, mut duplicated, mut hedges) = (0u64, 0u64, 0u32);
        let mut frame_two_admitted = None;
        for i in 1..=200u32 {
            b.extend(FRAME);
            let admitted = b.try_charge(FRAME / 2);
            if admitted {
                hedges += 1;
                duplicated += FRAME / 2;
                b.settle(FRAME / 2, FRAME / 2);
            }
            if i == 2 {
                frame_two_admitted = Some(admitted);
            }
            b.earn(FRAME);
            b.retire(FRAME);
            completed += FRAME;
            let bound = HEDGE_BUDGET_RATIO * completed as f64 + (FRAME / 2) as f64;
            assert!(
                duplicated as f64 <= bound,
                "frame {i}: {duplicated} duplicated against {bound}"
            );
        }
        assert_eq!(
            frame_two_admitted,
            Some(false),
            "frame 2 starts in debt for frame 1's whole hedge"
        );
        assert!(
            hedges >= 10,
            "the debt is repaid, so hedging comes back: {hedges}"
        );
    }

    /// The cap is the larger of the ratio term and the floor, and the crossover
    /// sits where the in-flight bytes are ten times the largest item.
    #[test]
    fn a_live_budget_cap_is_the_larger_of_the_ratio_and_the_floor() {
        let mut b = HedgeBudget::live();
        for _ in 0..10 {
            b.extend(1_000_000);
            assert!(close(b.cap(), 500_000.0), "the floor holds up to ten items: {}", b.cap());
        }
        // The eleventh item takes the ratio term (550 kB) past the floor.
        b.extend(1_000_000);
        assert!(close(b.cap(), 550_000.0), "{}", b.cap());
        assert!(close(b.available(), 550_000.0), "{}", b.available());

        // Many small items and one large: the ratio wins, then the floor, then
        // the ratio again.
        let mut b = budget_with(100, 100_000);
        assert!(close(b.cap(), 500_000.0), "ratio term, 5 % of 10 MB: {}", b.cap());
        b.extend(10_000_000);
        assert!(close(b.cap(), 5_000_000.0), "floor term, half of 10 MB: {}", b.cap());
        b.retire(10_000_000);
        assert!(close(b.cap(), 500_000.0), "back to the ratio term: {}", b.cap());
        assert!(b.available() <= b.cap());
    }

    /// Retiring the largest item drops the floor to the next largest; what is
    /// outstanding stays outstanding.
    #[test]
    fn a_live_budget_retiring_the_largest_item_lowers_the_floor() {
        let mut b = HedgeBudget::live();
        b.extend(4_000_000);
        b.extend(1_000_000);
        b.extend(1_000_000);
        assert!(close(b.cap(), 2_000_000.0), "{}", b.cap());
        assert!(b.try_charge(1_000_000));
        assert!(close(b.available(), 1_000_000.0), "{}", b.available());
        b.retire(4_000_000);
        assert!(close(b.cap(), 500_000.0), "half of the next largest: {}", b.cap());
        assert!(close(b.available(), 0.0), "the outstanding charge exceeds the new cap");
        b.settle(1_000_000, 0);
        assert!(close(b.available(), 500_000.0), "{}", b.available());
    }

    /// With nothing in flight there is no budget and nothing is admitted, and
    /// nothing earned or repaid changes that.
    #[test]
    fn a_live_budget_with_nothing_in_flight_admits_nothing() {
        let mut b = HedgeBudget::live();
        assert!(!b.try_charge(1), "an empty budget admits nothing");
        b.extend(2_000_000);
        b.retire(2_000_000);
        assert!(close(b.cap(), 0.0) && close(b.available(), 0.0));
        assert!(!b.try_charge(1), "a drained run admits nothing");
        b.earn(1_000_000);
        assert!(close(b.available(), 0.0), "nothing can be earned past a zero cap");
    }

    /// Items of equal size retire one at a time.
    #[test]
    fn a_live_budget_counts_equal_sized_items_separately() {
        let mut b = HedgeBudget::live();
        b.extend(1_000_000);
        b.extend(1_000_000);
        b.extend(100_000);
        assert!(close(b.cap(), 500_000.0), "{}", b.cap());
        b.retire(1_000_000);
        assert!(close(b.cap(), 500_000.0), "one 1 MB item is still in flight: {}", b.cap());
        b.retire(1_000_000);
        assert!(close(b.cap(), 50_000.0), "only the 100 kB item is left: {}", b.cap());
        assert!(b.available() <= b.cap());
    }

    /// A hedge charge that is dropped unsettled gives its bytes back and adds no
    /// debt; one that was settled does not settle again on drop.
    #[test]
    fn a_dropped_hedge_charge_returns_its_bytes_and_a_settled_one_does_not_settle_twice() {
        let mut b = budget_with(1, 1_000_000);
        assert!(b.try_charge(500_000));
        let ledger = ledger_of(b);
        let guard = HedgeCharge {
            ledger: Arc::clone(&ledger),
            bytes: 500_000,
            settled: false,
        };
        assert_eq!(ledger.lock().unwrap().budget.outstanding, 500_000);
        drop(guard);
        {
            let l = ledger.lock().unwrap();
            assert_eq!(l.budget.outstanding, 0, "the charge came back");
            assert!(close(l.budget.debt, 0.0), "and a dropped hedge duplicated nothing");
            assert_eq!(l.hedge_bytes, 0);
        }

        // Settled with debt, then dropped: the drop settles nothing more. A
        // second charge stays outstanding beside it, so a drop that settled
        // again would take that one's bytes and show here.
        {
            let mut l = ledger.lock().unwrap();
            l.budget.extend(1_000_000);
            assert!(l.budget.try_charge(200_000));
            assert!(l.budget.try_charge(200_000));
        }
        let mut guard = HedgeCharge {
            ledger: Arc::clone(&ledger),
            bytes: 200_000,
            settled: false,
        };
        guard.settle(123);
        drop(guard);
        let l = ledger.lock().unwrap();
        assert_eq!(l.budget.outstanding, 200_000, "the other charge is untouched");
        assert!(close(l.budget.debt, 123.0), "{}", l.budget.debt);
        assert_eq!(l.hedge_bytes, 123);
    }
}
