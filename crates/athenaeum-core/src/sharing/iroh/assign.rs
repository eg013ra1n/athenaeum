//! A2a — our own swarm assignment loop over `store.remote()` (D4 §4.5, T4).
//!
//! This module replaces iroh-blobs' `SplitStrategy::Split` fan-out for the
//! multi-source project fetch. The stock loop
//! (`iroh_blobs::api::downloader::execute_get`) is a strictly sequential
//! first-fit walk of the provider list per child: dial, `local_for_request`,
//! `execute_get_sink`, and on an `Err` `continue` to the next provider, asking
//! it only for `local.missing()`. That is sound and its byte-level resume is
//! free — but it switches providers **only on an error**, never on
//! uselessness, and it throws away the `get::Stats` every transfer produces.
//!
//! D4 §1(б) calls that the correctness defect: a peer that accepts the
//! connection and then dribbles 20 KB/s never errors, so the child request
//! never moves, and if it is the last child of a package the whole package
//! waits for it. This loop adds the two things the stock one cannot have:
//!
//! - a **progress deadline** per assignment — no growth in the request's
//!   `bytes_read` for [`STALL_HARD_LIMIT`] is a provider failure, whatever QUIC
//!   thinks (D4 §6, the `piece_timeout = 20 s` precedent);
//! - **per-provider accounting** ([`ProviderStats`]) — the ground truth the
//!   stock downloader discards, and the input A4's ranking will need.
//!
//! Everything else is deliberately the same as upstream, because it already
//! works: one `GetRequest` per collection child, the missing-range recomputed
//! from the local store before every attempt (so a reassignment resumes at the
//! byte, never restarts the frame), and the full provider set available to
//! every child.
//!
//! ## Independent items
//!
//! The engine runs over [`FetchItem`]s — each with its own `GetRequest`, hash
//! and provider list — through [`fetch_items_assigned`]. A collection is the
//! special case [`fetch_children_assigned`] builds: every child addressed
//! through its root, one shared provider list, the personal ALPN, and
//! [`FailMode::FailFast`]. A batch of collab frames is raw blob items with
//! per-frame holders on the collab ALPN under [`FailMode::Isolate`], where one
//! item's failure is its own result and never aborts a sibling. Per-provider
//! state stays run-wide either way: one pool, one ledger, one backoff ladder
//! per provider, whichever items it serves.
//!
//! ## Hedging
//!
//! Task 8 added the second, racing assignment: while a child's primary
//! transfer runs unhedged, it is re-evaluated every
//! [`HEDGE_REEVALUATE_INTERVAL`] against the trigger `max(p95 of recent
//! completions, [`HEDGE_EXPECTED_MULTIPLIER`] × expected)` — late by either
//! measure arms a hedge on a different provider for the BACK HALF of the
//! child's still-missing range, split at the midpoint
//! ([`split_missing_at_midpoint`]), under a gRPC-shaped [`HedgeBudget`] that
//! caps total spend at [`HEDGE_BUDGET_RATIO`] of the collection. Whichever
//! side finishes first wins the round; the loser is cancelled by dropping its
//! `GetProgress` stream (see Cancellation above), which is enough — no pooled
//! connection needs closing. A primary that fails while its hedge is still
//! live does not cancel the hedge: the hedge is a real transfer, not a
//! speculative copy, so it is promoted and carries the round alone
//! (`primary_gone`). See [`try_arm_hedge`] for the trigger and the budget
//! check, and [`HedgeBudget`] for the accounting.
//!
//! No persistence, no ranking: [`pick_provider`] is a deliberately dumb
//! least-loaded pick, kept as ONE function so A4's `RankedProviders` is one
//! edit.
//!
//! ## Progress truth
//!
//! Nothing here emits UI progress. D4 T7 fixed `store.observe()` as the single
//! progress oracle — the get streams are consumed here only to watch for
//! movement, and the `Progress(u64)` values never leave this module. The batch
//! figure the caller emits is summed from the per-file observers, which are
//! unaffected by which fan-out ran.
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
//! while leaving the connection warm for the next child — which is what we
//! want: a stalled peer is not necessarily a dead one, and closing its
//! connection would make the next attempt pay a fresh handshake.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use iroh::{Endpoint, EndpointId};
use iroh_blobs::api::blobs::Blobs;
use iroh_blobs::api::remote::{GetProgressItem, Remote};
use iroh_blobs::api::Store;
use iroh_blobs::get::Stats;
use iroh_blobs::protocol::{ChunkRanges, ChunkRangesExt, GetRequest};
use iroh_blobs::util::connection_pool::{ConnectionPool, Options as PoolOptions};
use iroh_blobs::Hash;
use n0_future::StreamExt as _;

use crate::sharing::{ProviderEvent, ProviderTelemetrySink};

/// Child requests in flight at once — the same figure the stock split fan-out
/// uses (`handle_download_split_impl`'s `buffered_unordered(32)`), so switching
/// modes does not change how hard we lean on the swarm.
pub(crate) const MAX_IN_FLIGHT: usize = 32;

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

/// Environment override for [`swarm_fetch_mode`].
const SWARM_FETCH_ENV: &str = "ATHENAEUM_SWARM_FETCH";

/// Which fan-out phase 2 of a multi-source collection fetch runs.
///
/// `Stock` is iroh-blobs' `SplitStrategy::Split` — kept reachable on purpose
/// (D4 §9: "switching between them is one flag"), because this module takes on
/// resume, cancellation and accounting that upstream otherwise owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwarmFetchMode {
    /// iroh-blobs `SplitStrategy::Split` — the fallback.
    Stock,
    /// This module's assignment loop — the default.
    Assigned,
}

/// The mode this process runs in, resolved ONCE from the environment.
///
/// `ATHENAEUM_SWARM_FETCH=stock` forces the upstream fan-out without a rebuild;
/// anything else (unset, or set empty, included) is
/// [`SwarmFetchMode::Assigned`], and an unrecognised value warns rather than
/// silently picking one. Read through
/// a `OnceLock` so a mid-run environment edit cannot make two fetches of the
/// same session disagree, and so the decision is logged exactly once.
pub(crate) fn swarm_fetch_mode() -> SwarmFetchMode {
    static MODE: OnceLock<SwarmFetchMode> = OnceLock::new();
    *MODE.get_or_init(|| match std::env::var(SWARM_FETCH_ENV) {
        Ok(v) if v.eq_ignore_ascii_case("stock") => {
            tracing::info!(mode = "stock", "swarm fetch mode overridden by environment");
            SwarmFetchMode::Stock
        }
        // An empty value is how a shell spells "unset"; it is not an unknown
        // mode and must not warn.
        Ok(v) if v.trim().is_empty() || v.eq_ignore_ascii_case("assigned") => {
            SwarmFetchMode::Assigned
        }
        Ok(v) => {
            tracing::warn!(
                mode = %v,
                "unknown swarm fetch mode in the environment — using the assigned loop"
            );
            SwarmFetchMode::Assigned
        }
        Err(_) => SwarmFetchMode::Assigned,
    })
}

/// Per-provider transfer facts — the ground truth the stock downloader throws
/// away (`Ok(_stats)`).
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

/// What one [`fetch_children_assigned`] call did, per provider plus the
/// swarm-wide counters.
///
/// `hedges` / `hedge_bytes` are Task 8's; they are reported as `0` here rather
/// than omitted so the shape does not move when hedging lands.
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

    /// Total children some provider carried to completion.
    ///
    /// Report-only: a child already complete at its first round (a resumed
    /// fetch finding it already local) or a duplicate-hash entry never runs
    /// `record_success`, so it is credited to nobody and this sum undercounts
    /// the collection's own child count by exactly that many. Nothing here
    /// re-derives that figure from the collection to true it up.
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
/// ([`HEDGE_BUDGET_RATIO`] × the bytes of every completed child) and what it is
/// capped at ([`HEDGE_BUDGET_RATIO`] × the whole collection), but not where it
/// starts; a bucket that starts empty can never pay for the very first hedge,
/// which is the one that matters on a package whose first assignment lands on a
/// useless peer. Starting full is also what a token bucket means everywhere
/// else, and the cap still bounds the total spend.
#[derive(Debug)]
pub(crate) struct HedgeBudget {
    tokens: f64,
    cap: f64,
}

impl HedgeBudget {
    fn new(total_bytes: u64) -> Self {
        let cap = HEDGE_BUDGET_RATIO * total_bytes as f64;
        Self { tokens: cap, cap }
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
    /// providers splitting the children evenly, where completed bytes are half
    /// the collection and the cap is 5 % of it — and a strict `>` refused every
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

/// Knobs for one assignment run.
#[derive(Clone)]
pub(crate) struct AssignmentOptions {
    /// D4 §6's hard progress deadline. Production always passes
    /// [`STALL_HARD_LIMIT`]; tests shorten it so a deliberately trickling peer
    /// trips it inside a test's patience.
    pub stall_hard_limit: Duration,
    /// A2b: race a second assignment for the back half of a slow child's
    /// missing range, under [`HedgeBudget`].
    pub hedging: bool,
    /// The collection's announced payload size — the base of the hedge
    /// budget's cap ([`HEDGE_BUDGET_RATIO`] × this). Zero disables hedging by
    /// arithmetic: a zero cap can never admit a charge.
    pub total_bytes: u64,
    /// Per-provider attempt telemetry, the same sink the stock path feeds from
    /// the download stream's `TryProvider`/`ProviderFailed` items.
    pub telemetry: ProviderTelemetrySink,
    /// The ALPN the run's one connection pool dials — `iroh_blobs::ALPN` for
    /// the personal store, [`COLLAB_BLOBS_ALPN`](super::COLLAB_BLOBS_ALPN) for
    /// the collab store. [`fetch_children_assigned`] forces the former.
    pub alpn: &'static [u8],
    /// What one item's failure does to the rest of the call.
    /// [`fetch_children_assigned`] forces [`FailMode::FailFast`].
    pub fail_mode: FailMode,
}

/// What one item exhausting its ladder does to the rest of the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailMode {
    /// The first failed item fails the whole call and aborts its siblings —
    /// the collection contract: a package missing one child is worthless.
    FailFast,
    /// A failed item records its error and the call goes on; every item gets
    /// its own result. Independent frames (P11): one frame failing never
    /// aborts its siblings.
    Isolate,
}

/// One independently fetched blob: its own request, its own provider list,
/// its own result.
///
/// `request` is `GetRequest::blob(hash)` for a raw blob, or
/// `GetRequest::builder().child(i, ChunkRanges::all()).build(root)` for a
/// collection child — the engine never builds a request itself, it only
/// narrows this one's ranges for a hedge ([`hedge_back_half_request`]).
/// `hash` is the blob the request lands (the child's own hash for a child
/// request), which the hedge observes. `size` is the caller's announced size,
/// informational only (`0` when unknown): the hedge budget's base is
/// [`AssignmentOptions::total_bytes`].
pub(crate) struct FetchItem {
    /// The caller's id for the item (a frame uuid; a child index for the
    /// collection wrapper), echoed in the results.
    pub key: String,
    pub request: GetRequest,
    pub hash: Hash,
    pub size: u64,
    /// The providers this item may be assigned to. Per-provider state
    /// (backoff, load, goodput) is shared across every item of the call.
    pub providers: Arc<Vec<EndpointId>>,
}

impl FetchItem {
    /// How error messages name the item. A child keeps the exact wording the
    /// collection path always used (`child {index} of {root}`).
    fn describe(&self) -> String {
        if self.request.ranges.is_blob() {
            format!("blob {} ({})", self.key, self.hash)
        } else {
            format!("child {} of {}", self.key, self.request.hash)
        }
    }
}

/// Per-item results of one call, in input order.
pub(crate) type ItemResults = Vec<(String, Result<()>)>;

#[cfg(test)]
thread_local! {
    /// Connection pools opened by [`run_items`] on this thread.
    ///
    /// Thread-local rather than a process-wide atomic on purpose: the lib's
    /// tests run in parallel, and several of them open pools, so a global
    /// counter's delta would be another test's noise. `#[tokio::test]` drives
    /// its future with `block_on` on the test's own thread, and the pool is
    /// opened in the call's synchronous prelude, so the thread IS the call.
    static POOLS_OPENED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test hook: pools opened on the calling thread so far.
#[cfg(test)]
pub(crate) fn pools_opened_on_this_thread() -> usize {
    POOLS_OPENED.with(|c| c.get())
}

/// The run's one connection pool on `alpn`.
fn open_pool(endpoint: &Endpoint, alpn: &'static [u8]) -> ConnectionPool {
    #[cfg(test)]
    POOLS_OPENED.with(|c| c.set(c.get() + 1));
    ConnectionPool::new(endpoint.clone(), alpn, PoolOptions::default())
}

/// Why one assignment ended without the child's bytes.
///
/// Both variants carry the bytes the attempt did move, because that is what
/// the provider is credited with and — Task 8 — what tells a hedge whether the
/// loser was worth anything.
#[derive(Debug)]
enum TransferFault {
    /// No growth in `bytes_read` for `stall_hard_limit`. No error was raised;
    /// this is the judgement the stock loop cannot make.
    Stalled { bytes: u64 },
    /// The dial or the transfer errored — the stock loop's own trigger.
    Failed { bytes: u64, error: anyhow::Error },
}

impl TransferFault {
    fn bytes(&self) -> u64 {
        match self {
            Self::Stalled { bytes } | Self::Failed { bytes, .. } => *bytes,
        }
    }

    fn is_stall(&self) -> bool {
        matches!(self, Self::Stalled { .. })
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
            Self::Failed { error, .. } => format!("{error:#}"),
        }
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

/// Fetch every `children` entry of the hash-sequence rooted at `root` from
/// `providers`, one assignment at a time per child, with a progress deadline.
///
/// `children` are hash-sequence child INDICES — for an iroh-blobs
/// [`Collection`](iroh_blobs::format::collection::Collection) the sequence is
/// `[meta, file0, file1, …]`, so file *i* is child `i + 1` (and
/// `GetRequest::builder().child(c, …)` is itself offset `c + 1`, the root
/// hash-seq blob being offset 0). The hash beside each index is carried for
/// logging only — the request addresses the child through `root`, exactly as
/// upstream's `split_request` does, so verification stays anchored on the
/// sequence we already hold.
///
/// Returns once every child is complete locally. The FIRST child that exhausts
/// its ladder fails the whole call and aborts its siblings; partial bytes stay
/// in the store (the caller's in-flight tag protects them), so a retry — even
/// against an entirely different provider set — resumes.
///
/// A thin wrapper over the item engine ([`fetch_items_assigned`]): each child
/// becomes a [`FetchItem`] keyed by its index and sharing ONE provider list,
/// on `iroh_blobs::ALPN` under [`FailMode::FailFast`] — both forced here,
/// whatever `opts` carries, so a collection can never be fetched isolated or
/// from the collab store by accident.
pub(crate) async fn fetch_children_assigned(
    store: &Store,
    endpoint: &Endpoint,
    providers: Vec<EndpointId>,
    root: Hash,
    children: Vec<(u64, Hash)>,
    opts: AssignmentOptions,
) -> Result<AssignmentReport> {
    anyhow::ensure!(
        !providers.is_empty(),
        "assignment loop needs at least one provider"
    );
    let providers = Arc::new(providers);
    let items = children
        .into_iter()
        .map(|(index, hash)| FetchItem {
            key: index.to_string(),
            request: GetRequest::builder()
                .child(index, ChunkRanges::all())
                .build(root),
            hash,
            // A collection child's size is not known here; the budget's base
            // is `opts.total_bytes`, the collection's announced size.
            size: 0,
            providers: Arc::clone(&providers),
        })
        .collect();
    let opts = AssignmentOptions {
        alpn: iroh_blobs::ALPN,
        fail_mode: FailMode::FailFast,
        ..opts
    };
    let (report, _results) = run_items(store, endpoint, items, opts, Some(root)).await?;
    Ok(report)
}

/// Fetch a set of INDEPENDENT items, each from its own provider list, in one
/// assignment run: one connection pool on `opts.alpn`, one hedge ledger, one
/// per-provider state shared by every item.
///
/// Per item, the behaviour is exactly the collection child's: a progress
/// deadline, the backoff ladder, hedging of the back half, and byte-level
/// resume from `local_for_request(item.request)`. `opts.fail_mode` decides
/// what a failed item does to the others; under [`FailMode::Isolate`] the call
/// itself only fails for a reason no item owns, and every item's outcome is in
/// the returned results, in input order.
#[allow(dead_code)] // consumed by the collab replication pass (wave 2, Task 9)
pub(crate) async fn fetch_items_assigned(
    store: &Store,
    endpoint: &Endpoint,
    items: Vec<FetchItem>,
    opts: AssignmentOptions,
) -> Result<(AssignmentReport, ItemResults)> {
    run_items(store, endpoint, items, opts, None).await
}

/// The engine behind both entry points. `batch_root` is the collection root
/// the collection wrapper logs its run under; `None` for a set of raw items.
async fn run_items(
    store: &Store,
    endpoint: &Endpoint,
    items: Vec<FetchItem>,
    opts: AssignmentOptions,
    batch_root: Option<Hash>,
) -> Result<(AssignmentReport, ItemResults)> {
    // Our own pool, not the downloader's: `Downloader` owns its pool privately
    // and hands out no handle. Same defaults (1 s connect timeout, 5 s idle) —
    // a connection stays warm across the items assigned to one provider, which
    // is the whole reason to pool at all. ONE pool per call, whatever the item
    // count.
    let pool = open_pool(endpoint, opts.alpn);
    let remote: Remote = store.remote().clone();
    let blobs: Blobs = store.blobs().clone();

    // One hedge ledger for the run: the budget, the p95 window and the report
    // counters. `total_bytes` is the batch's announced payload size, so the
    // cap is 5 % of what this fetch is worth — not of what one item is.
    let ledger: Ledger = Arc::new(Mutex::new(HedgeLedger {
        budget: HedgeBudget::new(opts.total_bytes),
        completions: CompletionWindow::default(),
        hedges: 0,
        hedge_bytes: 0,
    }));

    // Per-provider state is GLOBAL to the run — the union of every item's
    // providers — so a provider's backoff, load and goodput follow it across
    // items; each item only chooses among its own list.
    let states: States = Arc::new(Mutex::new(
        items
            .iter()
            .flat_map(|item| item.providers.iter().copied())
            .map(|p| (p, ProviderState::default()))
            .collect(),
    ));

    let item_count = items.len();
    let labels: Vec<(String, u64)> = items.iter().map(|i| (i.key.clone(), i.size)).collect();
    let mut results: Vec<Option<Result<()>>> = (0..item_count).map(|_| None).collect();
    let mut slot_of: HashMap<tokio::task::Id, usize> = HashMap::new();
    let mut pending = items.into_iter().enumerate();
    let mut set = tokio::task::JoinSet::new();

    // Spawn and drain in the SAME loop rather than spawning all items behind a
    // semaphore: a permit taken before `spawn` bounds concurrency just as well,
    // but it leaves every not-yet-spawned item queued behind a swarm that is
    // already failing, so a dead swarm would walk the backoff ladder once per
    // item instead of once. Draining here also means the first item's error
    // reaches us immediately, and under `FailFast` dropping the `JoinSet` on
    // the way out aborts every sibling.
    loop {
        while set.len() < MAX_IN_FLIGHT {
            let Some((slot, item)) = pending.next() else {
                break;
            };
            if item.providers.is_empty() {
                let e = anyhow::anyhow!("{}: no provider to assign it to", item.describe());
                match opts.fail_mode {
                    FailMode::FailFast => return Err(fail_with_report(&states, batch_root, e)),
                    FailMode::Isolate => {
                        results[slot] = Some(Err(isolated_failure(&labels[slot], e)));
                        continue;
                    }
                }
            }
            let handle = set.spawn(run_child(
                pool.clone(),
                remote.clone(),
                blobs.clone(),
                Arc::clone(&states),
                Arc::clone(&ledger),
                item,
                opts.clone(),
            ));
            slot_of.insert(handle.id(), slot);
        }
        let (id, outcome) = match set.join_next_with_id().await {
            Some(Ok((id, outcome))) => (id, outcome),
            Some(Err(join)) => (
                join.id(),
                Err(anyhow::Error::new(join).context("assignment task panicked")),
            ),
            None => break,
        };
        let slot = slot_of
            .remove(&id)
            .expect("every spawned assignment task is registered under its id");
        match outcome {
            Ok(()) => {
                if opts.fail_mode == FailMode::Isolate {
                    let (key, bytes) = &labels[slot];
                    tracing::debug!(frame_uuid = %key, bytes = *bytes, outcome = "ok", "blob fetch finished");
                }
                results[slot] = Some(Ok(()));
            }
            Err(e) => match opts.fail_mode {
                FailMode::FailFast => return Err(fail_with_report(&states, batch_root, e)),
                FailMode::Isolate => {
                    results[slot] = Some(Err(isolated_failure(&labels[slot], e)));
                }
            },
        }
    }

    let report = report_from_with_ledger(&states, &ledger);
    tracing::debug!(
        root_hash = batch_root.map(tracing::field::display),
        count = item_count,
        providers = report.per_provider.len(),
        stalls = report.stalls,
        hedges = report.hedges,
        bytes = report.total_bytes(),
        "assignment loop finished"
    );
    let results = labels
        .into_iter()
        .zip(results)
        .map(|((key, _), r)| {
            (
                key,
                r.unwrap_or_else(|| Err(anyhow::anyhow!("assignment item produced no result"))),
            )
        })
        .collect();
    Ok((report, results))
}

/// Log one item's failure under [`FailMode::Isolate`] and hand the error back
/// for its result slot.
fn isolated_failure((key, bytes): &(String, u64), e: anyhow::Error) -> anyhow::Error {
    tracing::warn!(frame_uuid = %key, error = %format!("{e:#}"), "blob fetch failed");
    tracing::debug!(frame_uuid = %key, bytes = *bytes, outcome = "failed", "blob fetch finished");
    e
}

/// Log the per-provider picture the run ended on, and give the caller's error
/// the provider fault that most recently explained it.
///
/// A bare "every provider exhausted" says nothing a user or a log reader can
/// act on; the swarm's own last real cause (a refused dial, a reset stream)
/// does, and it is already on the state we are about to drop.
fn fail_with_report(states: &States, root: Option<Hash>, err: anyhow::Error) -> anyhow::Error {
    let report = report_from(states);
    let cause = last_failure_cause(states);
    tracing::error!(
        root_hash = root.map(tracing::field::display),
        providers = report.per_provider.len(),
        stalls = report.stalls,
        bytes = report.total_bytes(),
        count = report.total_children(),
        error = %cause.as_deref().unwrap_or("no provider reported an error (every failure was a stall)"),
        "assignment loop failed"
    );
    match cause {
        Some(c) => err.context(format!("last provider fault: {c}")),
        None => err,
    }
}

/// The most recently recorded `TransferFault::Failed` cause across providers.
fn last_failure_cause(states: &States) -> Option<String> {
    let guard = states.lock().expect("assignment states mutex poisoned");
    guard
        .values()
        .filter_map(|st| Some((st.last_error_at?, st.last_error.clone()?)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, cause)| cause)
}

/// One child: pick a provider, transfer what is still missing, repeat until the
/// child is complete locally or the ladder runs out.
#[allow(clippy::too_many_arguments)]
async fn run_child(
    pool: ConnectionPool,
    remote: Remote,
    blobs: Blobs,
    states: States,
    ledger: Ledger,
    item: FetchItem,
    opts: AssignmentOptions,
) -> Result<()> {
    let request = item.request.clone();
    // Log fields keep the collection path's names: `root_hash` is the hash the
    // request addresses (the collection root, or the raw blob itself), `child`
    // is the item's key (the child index for a collection).
    let root = item.request.hash;
    let index = item.key.as_str();
    let hash = item.hash;
    let providers = Arc::clone(&item.providers);
    let mut rounds = 0u32;
    // Set when a hedge wins: the rest of this child goes to the provider that
    // just proved it can move bytes, not back to the one we gave up on.
    let mut forced: Option<EndpointId> = None;
    'child: loop {
        // Recomputed EVERY round, so a reassignment asks the next provider only
        // for the bytes still missing — the byte-level resume upstream gets for
        // free and we must not lose.
        let local = remote
            .local_for_request(request.clone())
            .await
            .map_err(|e| anyhow::anyhow!("local info for {}: {e}", item.describe()))?;
        if local.is_complete() {
            return Ok(());
        }
        let missing = local.missing();

        let Some(claim) = claim_provider(&states, &providers, forced.take()) else {
            // Every provider is in backoff. Wait for the earliest of them and
            // try again — bounded, never a spin.
            rounds += 1;
            if rounds > MAX_BACKOFF_ROUNDS {
                anyhow::bail!(
                    "{}: every provider exhausted after {rounds} rounds",
                    item.describe()
                );
            }
            let wait = earliest_wait(&states);
            tracing::debug!(
                root_hash = %root,
                child = index,
                attempt = rounds,
                delay_ms = wait.as_millis() as u64,
                "every provider in backoff — waiting for the earliest"
            );
            tokio::time::sleep(wait).await;
            continue;
        };

        let provider = claim.provider();
        (opts.telemetry)(ProviderEvent::Trying(*provider.as_bytes()));
        let started = Instant::now();

        // ── the race ────────────────────────────────────────────────────────
        // The primary keeps the whole missing range. A hedge, once the trigger
        // and the budget allow one, takes the BACK half of what is still
        // missing from a different provider; whichever side finishes, the other
        // is dropped, which resets its QUIC stream (see the module doc).
        let primary_progress = Arc::new(AtomicU64::new(0));
        let mut primary: BoxedTransfer = Box::pin(transfer_once(
            pool.clone(),
            remote.clone(),
            provider,
            missing,
            opts.stall_hard_limit,
            Arc::clone(&primary_progress),
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
                    record_failure(&states, provider, &fault, started.elapsed());
                    (opts.telemetry)(ProviderEvent::Failed(*provider.as_bytes()));
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
                            record_failure(&states, h.provider, &fault, h.started.elapsed());
                            (opts.telemetry)(ProviderEvent::Failed(*h.provider.as_bytes()));
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
                        &pool,
                        &remote,
                        &blobs,
                        &states,
                        &ledger,
                        &opts,
                        &item,
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
                record_failure(&states, provider, &fault, elapsed);
                (opts.telemetry)(ProviderEvent::Failed(*provider.as_bytes()));
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
                }
                // Round again: `local_for_request` above recomputes the missing
                // range, so the next provider resumes at the byte.
            }
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
/// `GetRequest::blob_ranges(hash, back)`; a collection child
/// (`child(i).build(root)`, i.e. one non-empty range at offset `i + 1`) keeps
/// the builder shape the collection path always sent,
/// `child(i, back).build(root)`. Any other shape is not an item this engine
/// builds, and gets no hedge (`None`) rather than a guessed request.
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
    pool: &ConnectionPool,
    remote: &Remote,
    blobs: &Blobs,
    states: &States,
    ledger: &Ledger,
    opts: &AssignmentOptions,
    item: &FetchItem,
    primary: EndpointId,
    started: Instant,
    primary_progress: &Arc<AtomicU64>,
) -> Option<HedgeRun> {
    let providers: &[EndpointId] = &item.providers;
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
    let fut: BoxedTransfer = Box::pin(transfer_once(
        pool.clone(),
        remote.clone(),
        hedge_provider,
        request,
        opts.stall_hard_limit,
        Arc::clone(&progress),
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
/// how far it got. Takes its pool and remote by value so the future is
/// `'static` and can be boxed beside a sibling in a `select!`. In the stall case the
/// `GetProgress` stream is dropped on the way out, which resets the QUIC stream
/// — see the module doc's cancellation note for why that is enough.
async fn transfer_once(
    pool: ConnectionPool,
    remote: Remote,
    provider: EndpointId,
    request: GetRequest,
    stall_hard_limit: Duration,
    progress: Arc<AtomicU64>,
) -> std::result::Result<Stats, TransferFault> {
    let conn = pool
        .get_or_connect(provider)
        .await
        .map_err(|e| TransferFault::Failed {
            bytes: 0,
            error: anyhow::anyhow!("dial {}: {e}", provider.fmt_short()),
        })?;

    // `ConnectionRef` derefs to the pooled `Connection` and holds the pool's
    // permit; it stays alive for the whole transfer and is dropped with this
    // function, releasing the permit while leaving the connection warm.
    let get = remote.execute_get((*conn).clone(), request);
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
                    last_bytes = b;
                    last_growth = Instant::now();
                    // Published so the caller can still read this assignment's
                    // progress after it has CANCELLED it — a cancelled future
                    // returns nothing, and the refund needs to know how much of
                    // the charged range the loser actually moved.
                    progress.store(b, Ordering::Relaxed);
                }
            }
            Ok(Some(GetProgressItem::Done(stats))) => return Ok(stats),
            Ok(Some(GetProgressItem::Error(e))) => {
                return Err(TransferFault::Failed {
                    bytes: last_bytes,
                    error: anyhow::anyhow!("get from {}: {e}", provider.fmt_short()),
                })
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
        let forced = forced.filter(|p| {
            guard
                .get(p)
                .is_some_and(|st| !st.next_try.is_some_and(|t| t > now))
        });
        let chosen = match forced {
            Some(p) => p,
            None => pick_provider(&guard, providers, now)?,
        };
        if let Some(st) = guard.get_mut(&chosen) {
            st.inflight += 1;
        }
        chosen
    };
    Some(InflightGuard {
        states: Arc::clone(states),
        provider: chosen,
    })
}

/// A2's provider choice: among the providers not in backoff, the least loaded;
/// ties go to the fewest failures, then to list order.
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
            let st = states.get(id)?;
            if st.next_try.is_some_and(|t| t > now) {
                return None;
            }
            Some((st.inflight, st.failures, order, *id))
        })
        .min()
        .map(|(_, _, _, id)| id)
}

/// How long until the earliest provider leaves backoff.
fn earliest_wait(states: &States) -> Duration {
    let now = Instant::now();
    let guard = states.lock().expect("assignment states mutex poisoned");
    guard
        .values()
        .filter_map(|st| st.next_try)
        .map(|t| t.saturating_duration_since(now))
        .min()
        .unwrap_or(BACKOFF_BASE)
        .max(MIN_BACKOFF_SLEEP)
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
    } else if let TransferFault::Failed { error, .. } = fault {
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
}
