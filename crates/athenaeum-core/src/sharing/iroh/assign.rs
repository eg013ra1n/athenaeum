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
//! ## What this is NOT (yet)
//!
//! No hedging. [`AssignmentOptions::hedging`] exists and is read nowhere —
//! Task 8 extends this module with the second, racing assignment and the token
//! bucket, and [`TransferFault`] already carries the bytes that decision needs.
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use iroh::{Endpoint, EndpointId};
use iroh_blobs::api::remote::{GetProgressItem, Remote};
use iroh_blobs::api::Store;
use iroh_blobs::get::Stats;
use iroh_blobs::protocol::{ChunkRanges, GetRequest};
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
    /// Task 8's; reported as `0` here so the shape does not move when hedging
    /// lands.
    #[allow(dead_code)]
    pub hedges: u32,
    #[allow(dead_code)]
    pub hedge_bytes: u64,
}

impl AssignmentReport {
    /// Total payload bytes accounted to some provider.
    pub(crate) fn total_bytes(&self) -> u64 {
        self.per_provider.values().map(|s| s.bytes).sum()
    }

    /// Total children some provider carried to completion.
    pub(crate) fn total_children(&self) -> u32 {
        self.per_provider.values().map(|s| s.children).sum()
    }
}

/// Knobs for one assignment run.
#[derive(Clone)]
pub(crate) struct AssignmentOptions {
    /// D4 §6's hard progress deadline. Production always passes
    /// [`STALL_HARD_LIMIT`]; tests shorten it so a deliberately trickling peer
    /// trips it inside a test's patience.
    pub stall_hard_limit: Duration,
    /// Task 8. Read nowhere yet — the seam, so hedging is an extension of this
    /// module rather than a rewrite of it.
    #[allow(dead_code)]
    pub hedging: bool,
    /// Per-provider attempt telemetry, the same sink the stock path feeds from
    /// the download stream's `TryProvider`/`ProviderFailed` items.
    pub telemetry: ProviderTelemetrySink,
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
    /// Stalls this provider caused — a strict subset of `stats.failures`, kept
    /// beside the reported stats because [`ProviderStats`]' shape is the
    /// brief's and a stall is a swarm-wide figure, not a per-provider one.
    stalls: u32,
    stats: ProviderStats,
}

type States = Arc<Mutex<HashMap<EndpointId, ProviderState>>>;

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

    // Our own pool, not the downloader's: `Downloader` owns its pool privately
    // and hands out no handle. Same ALPN, same defaults (1 s connect timeout,
    // 5 s idle) — a connection stays warm across the children assigned to one
    // provider, which is the whole reason to pool at all.
    let pool = ConnectionPool::new(endpoint.clone(), iroh_blobs::ALPN, PoolOptions::default());
    let remote: Remote = store.remote().clone();
    let providers = Arc::new(providers);

    let states: States = Arc::new(Mutex::new(
        providers
            .iter()
            .map(|p| (*p, ProviderState::default()))
            .collect(),
    ));

    let child_count = children.len();
    let mut pending = children.into_iter();
    let mut set = tokio::task::JoinSet::new();

    // Spawn and drain in the SAME loop rather than spawning all children behind
    // a semaphore: a permit taken before `spawn` bounds concurrency just as
    // well, but it leaves every not-yet-spawned child queued behind a swarm
    // that is already failing, so a dead swarm would walk the backoff ladder
    // once per child instead of once. Draining here also means the first
    // child's error reaches us immediately, and dropping the `JoinSet` on the
    // way out aborts every sibling.
    loop {
        while set.len() < MAX_IN_FLIGHT {
            let Some((index, hash)) = pending.next() else {
                break;
            };
            set.spawn(run_child(
                pool.clone(),
                remote.clone(),
                Arc::clone(&states),
                Arc::clone(&providers),
                root,
                index,
                hash,
                opts.clone(),
            ));
        }
        match set.join_next().await {
            Some(res) => res??,
            None => break,
        }
    }

    let report = report_from(&states);
    tracing::debug!(
        root_hash = %root,
        count = child_count,
        providers = report.per_provider.len(),
        stalls = report.stalls,
        bytes = report.total_bytes(),
        "assignment loop finished"
    );
    Ok(report)
}

/// One child: pick a provider, transfer what is still missing, repeat until the
/// child is complete locally or the ladder runs out.
#[allow(clippy::too_many_arguments)]
async fn run_child(
    pool: ConnectionPool,
    remote: Remote,
    states: States,
    providers: Arc<Vec<EndpointId>>,
    root: Hash,
    index: u64,
    hash: Hash,
    opts: AssignmentOptions,
) -> Result<()> {
    let request = GetRequest::builder()
        .child(index, ChunkRanges::all())
        .build(root);
    let mut rounds = 0u32;
    loop {
        // Recomputed EVERY round, so a reassignment asks the next provider only
        // for the bytes still missing — the byte-level resume upstream gets for
        // free and we must not lose.
        let local = remote
            .local_for_request(request.clone())
            .await
            .map_err(|e| anyhow::anyhow!("local info for child {index} of {root}: {e}"))?;
        if local.is_complete() {
            return Ok(());
        }
        let missing = local.missing();

        let Some(provider) = claim_provider(&states, &providers) else {
            // Every provider is in backoff. Wait for the earliest of them and
            // try again — bounded, never a spin.
            rounds += 1;
            if rounds > MAX_BACKOFF_ROUNDS {
                anyhow::bail!(
                    "child {index} of {root}: every provider exhausted after {rounds} rounds"
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

        (opts.telemetry)(ProviderEvent::Trying(*provider.as_bytes()));
        let started = Instant::now();
        let outcome = transfer_once(&pool, &remote, provider, missing, opts.stall_hard_limit).await;
        let elapsed = started.elapsed();
        release_inflight(&states, provider);

        match outcome {
            // A completed `execute_get` means the requested range is decoded,
            // BLAKE3-verified and imported, so the child is complete — the same
            // conclusion upstream's `execute_get` draws from the same `Ok`. We
            // deliberately do not spend another `local_for_request` to re-prove
            // it: the caller exports every child right after this returns, so an
            // incomplete one fails loudly there rather than passing silently.
            Ok(stats) => {
                record_success(&states, provider, &stats, elapsed);
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

/// One `execute_get` with a progress watchdog.
///
/// Returns as soon as the request completes, errors, or goes
/// `stall_hard_limit` without its `bytes_read` growing. In the stall case the
/// `GetProgress` stream is dropped on the way out, which resets the QUIC stream
/// — see the module doc's cancellation note for why that is enough.
async fn transfer_once(
    pool: &ConnectionPool,
    remote: &Remote,
    provider: EndpointId,
    request: GetRequest,
    stall_hard_limit: Duration,
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
    let progress = remote.execute_get((*conn).clone(), request);
    let mut stream = std::pin::pin!(progress.stream());

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
fn claim_provider(states: &States, providers: &[EndpointId]) -> Option<EndpointId> {
    let mut guard = states.lock().expect("assignment states mutex poisoned");
    let chosen = pick_provider(&guard, providers, Instant::now())?;
    if let Some(st) = guard.get_mut(&chosen) {
        st.inflight += 1;
    }
    Some(chosen)
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

fn release_inflight(states: &States, provider: EndpointId) {
    let mut guard = states.lock().expect("assignment states mutex poisoned");
    if let Some(st) = guard.get_mut(&provider) {
        st.inflight = st.inflight.saturating_sub(1);
    }
}

fn record_success(states: &States, provider: EndpointId, stats: &Stats, elapsed: Duration) {
    let mut guard = states.lock().expect("assignment states mutex poisoned");
    if let Some(st) = guard.get_mut(&provider) {
        // D4 §6: a success resets the consecutive-failure count AND clears the
        // backoff, so one good transfer readmits a provider immediately.
        st.failures = 0;
        st.next_try = None;
        st.stats.bytes += stats.payload_bytes_read;
        st.stats.children += 1;
        st.stats.elapsed += elapsed;
    }
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
fn record_failure(states: &States, provider: EndpointId, fault: &TransferFault, elapsed: Duration) {
    let now = Instant::now();
    let mut guard = states.lock().expect("assignment states mutex poisoned");
    let Some(st) = guard.get_mut(&provider) else {
        return;
    };
    st.stats.failures += 1;
    st.stats.bytes += fault.bytes();
    st.stats.elapsed += elapsed;
    if fault.is_stall() {
        st.stalls += 1;
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
    /// point, so a constant-byte array is rejected.
    fn id(_tag: u8) -> EndpointId {
        iroh::SecretKey::generate().public()
    }

    fn states_of(entries: Vec<(EndpointId, ProviderState)>) -> HashMap<EndpointId, ProviderState> {
        entries.into_iter().collect()
    }

    #[test]
    fn pick_provider_prefers_the_least_loaded_then_fewest_failures_then_order() {
        let (a, b, c) = (id(1), id(2), id(3));
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
        let (a, b) = (id(1), id(2));
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
        let a = id(1);
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
        record_success(&states, a, &Stats::default(), Duration::from_millis(1));
        let g = states.lock().unwrap();
        assert!(g[&a].next_try.is_none(), "a success clears the backoff");
        assert_eq!(g[&a].failures, 0, "and resets the consecutive count");
        assert_eq!(g[&a].stats.children, 1);
    }

    #[test]
    fn backoff_rungs_double_and_stop_at_the_cap() {
        let a = id(1);
        let states: States = Arc::new(Mutex::new(states_of(vec![(a, ProviderState::default())])));
        let fault = || TransferFault::Failed {
            bytes: 0,
            error: anyhow::anyhow!("dead"),
        };
        let mut seen = Vec::new();
        for _ in 0..(EVICT_AFTER_FAILURES + BACKOFF_MAX_RUNGS + 3) {
            // Force each failure into its own window by clearing the deadline,
            // which is what elapsing it does in the real loop.
            states.lock().unwrap().get_mut(&a).unwrap().next_try = None;
            record_failure(&states, a, &fault(), Duration::ZERO);
            if let Some(t) = states.lock().unwrap()[&a].next_try {
                seen.push(t.saturating_duration_since(Instant::now()).as_millis() as u64);
            }
        }
        // 500, 1000, 2000, 4000, 8000, 16000, then flat at the cap.
        let rungs: Vec<u64> = seen.iter().map(|ms| (ms + 50) / 100 * 100).collect();
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
