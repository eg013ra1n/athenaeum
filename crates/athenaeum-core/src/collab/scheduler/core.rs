//! The receive-side scheduler as a pure, deterministic core (spec §7.1):
//! hub events, disk events, fetch results and timers in; fetch, cancel and
//! lane commands out. The executor (`api::collab_live`, Task 15) performs
//! the commands and re-checks every precondition against the database; the
//! landing fence stays a DB conditional (I1). BTreeMaps and one seeded RNG
//! make every run reproducible (§12). No I/O and no logging happen here —
//! the executor logs the commands it performs.
//!
//! **Fetch ids.** Every `Start` mints a new `fetch_id`; `UpdateProviders`,
//! `Cancel` and `Finished` name it. The core owns "one fetch per frame": a
//! `Finished` whose id is not the fetch in flight for that frame (the late
//! result of a fetch the core cancelled, possibly already restarted at the
//! same version) is ignored, so the executor may forward every result.
//!
//! **Presence and live sets** (controller ruling, §7.4/§4.2). A provider
//! that leaves a frame's fed list (presence, membership, claim) leaves that
//! fetch's LIVE set — the engine gives it no new round — but a transfer
//! already running on it is never cut by the core; only its connection
//! closing ends it. After a hub restart the 30 s presence warm-up may empty
//! the lists for a while; fetches simply wait it out.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::collab::live::backoff::Backoff;
use crate::geometry::ransac::SplitMix64;

/// The largest work unit (spec §7.2): one frame of at most 256 MiB. The
/// executor hands it to the live run as its yield cut (`unit_cap_bytes`).
pub const WORK_UNIT_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// A frame wanted this long jumps the queue (spec §7.2), so nothing starves.
pub const STARVATION: Duration = Duration::from_secs(3600);

/// `(project_id, frame_uuid)`.
pub type FrameKey = (String, String);

/// One frame of a project's need set, pinned to its current version (I1).
/// `since_ms` is when the frame became wanted at this version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub key: FrameKey,
    pub content_version: i32,
    pub blake3: String,
    pub byte_size: i64,
    pub since_ms: i64,
}

/// A device worth dialing for a frame, with the relay to reach it through.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderRef {
    pub device: String,
    pub relay_url: Option<String>,
}

/// How a fetch ended, as the executor reports it. `Failed` covers every
/// failure the live run reports, including "refused by every provider"
/// (`RefusedByEveryProvider`): the frame is retried after its own back-off
/// and never dropped. `Cancelled` (a cut at a yield, a stale landing) is
/// retried at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchResult {
    Landed,
    Failed,
    Cancelled,
    AwaitingGc,
}

/// Why the core cancelled a fetch in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// The manifest moved to another version (I6, §7.4).
    NewVersion,
    /// The frame left the need set (exclusion, policy, quarantine, …).
    NotWanted,
    ProjectGone,
    StorageUnavailable,
    /// The fetch has no provider left and none can still be transferring,
    /// while another frame with a provider needs its slot. The frame stays
    /// wanted and restarts when a provider appears; its partial bytes stay
    /// in the store for the resume.
    ///
    /// "May still be transferring" is every device the fetch was given
    /// whose connection has not ended since: a device's claim on it ends on
    /// [`Input::DialFailed`], [`Input::ConnectionClosed`] or
    /// [`Input::ConnectionIdle`]. The executor (T15) must therefore feed
    /// EVERY close of a pooled connection — an idle close by our own pool
    /// as `ConnectionIdle`, any other close as `ConnectionClosed` — and
    /// `DialOk` after every successful dial.
    NoProvider,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// The project's whole need set (a replacement). A fetch in flight for a
    /// frame that left it is cancelled (`NotWanted`), one whose version or
    /// hash moved is cancelled (`NewVersion`) — both in this step. Provider
    /// lists survive only for frames in the set, and only the list of the
    /// want's own version; the executor feeds a frame's list whenever it
    /// (re-)enters the set, before or after this input — the order does not
    /// matter (R1).
    NeedSet {
        project_id: String,
        wants: Vec<Want>,
    },
    /// A frame's candidates (I4, I5), derived for `content_version` /
    /// `blake3` — the manifest's current version and hash when the executor
    /// derived them. Lists are kept per version: a Start uses only the list
    /// of the want's exact version AND hash, so a late list of another
    /// version is never used for it, a version that goes DOWN (an epoch
    /// restore, spec §4.4) finds its list when the need set follows, and a
    /// version NUMBER reused after a restore with another hash never
    /// inherits the old list.
    Providers {
        key: FrameKey,
        content_version: i32,
        blake3: String,
        providers: Vec<ProviderRef>,
    },
    /// Several frames' lists in one step: each entry exactly as
    /// [`Input::Providers`], in order, then ONE reconcile and ONE schedule
    /// (Task 15). A presence change of a device that holds thousands of
    /// frames, or an epoch change's re-feed, is one step instead of
    /// thousands — each of which would re-rank the whole need set.
    ProvidersBatch {
        lists: Vec<ProviderList>,
    },
    ProjectGone {
        project_id: String,
    },
    Storage {
        fetching: bool,
    },
    /// `collab.max_receive_streams`.
    Slots(usize),
    /// The collab ReceiveGate permit is held (`true`) / was yielded
    /// (`false`, a personal transfer waits). After a yield the core starts
    /// nothing, emits `ReleaseLane` once nothing is in flight (the permit
    /// goes back, spec §8) and only then `RequestLane` if work waits. Only
    /// `admitted: true` answers a `RequestLane`; a `false` while the core
    /// holds no permit (e.g. a late yield signal while a request is
    /// pending) changes nothing — so one request is never answered twice.
    Lane {
        admitted: bool,
    },
    /// A fetch ended. Ignored unless `fetch_id` is the fetch in flight for
    /// `key` (see the module doc), so the executor may forward every result.
    Finished {
        key: FrameKey,
        fetch_id: u64,
        result: FetchResult,
    },
    /// A dial failed (§7.3): the device backs off (1 s → 60 s, full jitter)
    /// and leaves every live provider set until then (I5).
    DialFailed {
        device: String,
    },
    /// A pooled connection closed for any reason but our own idle timer:
    /// as [`Input::DialFailed`].
    ConnectionClosed {
        device: String,
    },
    /// Our own pool closed the device's connection because it sat idle: no
    /// transfer can be running on it any more, but nothing went wrong — no
    /// back-off, the device stays in every live set.
    ConnectionIdle {
        device: String,
    },
    /// A dial succeeded: the device's back-off ends, and it may be sending
    /// again to every fetch whose live set holds it (after an idle close
    /// the engine dials it anew).
    DialOk {
        device: String,
    },
    /// Sync now (L10): every back-off ends. T15 feeds it whenever
    /// `collab::live::backoff::reset_all` fires.
    ClearBackoffs,
    Tick,
}

/// One entry of [`Input::ProvidersBatch`]: the fields of
/// [`Input::Providers`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderList {
    pub key: FrameKey,
    pub content_version: i32,
    pub blake3: String,
    pub providers: Vec<ProviderRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start {
        key: FrameKey,
        fetch_id: u64,
        content_version: i32,
        blake3: String,
        byte_size: i64,
        providers: Vec<ProviderRef>,
    },
    /// The live provider set of the fetch in flight.
    UpdateProviders {
        key: FrameKey,
        fetch_id: u64,
        providers: Vec<ProviderRef>,
    },
    Cancel {
        key: FrameKey,
        fetch_id: u64,
        reason: CancelReason,
    },
    RequestLane,
    ReleaseLane,
}

#[derive(Debug, Clone)]
struct InFlight {
    fetch_id: u64,
    content_version: i32,
    blake3: String,
    /// The live provider set as last commanded.
    providers: Vec<ProviderRef>,
    /// Every device this fetch was given that may still be sending to it:
    /// a device leaves only when its connection ends (a failed dial, a
    /// closed or an idle-closed connection) — never on a presence change
    /// (I5).
    maybe_active: BTreeSet<String>,
}

/// One back-off: the instant it ends and the full-jitter envelope (P3).
struct Retry {
    until_ms: i64,
    backoff: Backoff,
}

/// Strike `k`: grow its back-off from `now_ms`. A first strike draws the
/// back-off's seed from the core's RNG, so a run is reproducible (§12).
fn strike<K: Ord>(map: &mut BTreeMap<K, Retry>, rng: &mut SplitMix64, k: K, now_ms: i64) {
    let r = map.entry(k).or_insert_with(|| Retry {
        until_ms: 0,
        backoff: Backoff::with_seed(rng.next_u64()),
    });
    r.until_ms = now_ms.saturating_add(r.backoff.next_delay().as_millis() as i64);
}

fn backing_off<K: Ord + ?Sized, Q: Ord>(map: &BTreeMap<Q, Retry>, k: &K, now_ms: i64) -> bool
where
    Q: std::borrow::Borrow<K>,
{
    map.get(k).is_some_and(|r| r.until_ms > now_ms)
}

/// The scheduler's whole state (spec §7.1). See [`Core::step`].
pub struct Core {
    rng: SplitMix64,
    slots: usize,
    /// The collab lane (ReceiveGate permit) is held and not yielded.
    lane: bool,
    /// The lane was yielded while fetches were in flight: the permit is
    /// still held until they drain, then `ReleaseLane`.
    yielded: bool,
    /// A `RequestLane` is outstanding (answered by `Lane`).
    lane_requested: bool,
    storage_ok: bool,
    /// The `now_ms` of the latest step (for [`Core::next_wake_ms`]).
    now_ms: i64,
    next_fetch_id: u64,
    wants: BTreeMap<FrameKey, Want>,
    /// The per-want random draw behind "rarest first, then random" (§7.2).
    tiebreak: BTreeMap<FrameKey, u64>,
    /// Per frame, per `(content_version, blake3)` it was derived for: the
    /// list last fed (R1). Keyed by the hash too, so a late list of the old
    /// content of a reused version number never overwrites the new one.
    providers: BTreeMap<FrameKey, BTreeMap<(i32, String), Vec<ProviderRef>>>,
    in_flight: BTreeMap<FrameKey, InFlight>,
    /// Per provider device: a failed dial or a closed connection (§7.3).
    dial_backoff: BTreeMap<String, Retry>,
    /// Per frame: a failed fetch (incl. refused by every provider).
    frame_backoff: BTreeMap<FrameKey, Retry>,
}

impl Core {
    pub fn new(seed: u64, slots: usize) -> Self {
        let mut rng = SplitMix64(seed);
        // ids of a replaced core never collide with this one's (never 0)
        let first_fetch_id = rng.next_u64().max(1);
        Self {
            rng,
            slots: slots.max(1),
            lane: false,
            yielded: false,
            lane_requested: false,
            storage_ok: true,
            now_ms: i64::MIN,
            next_fetch_id: first_fetch_id,
            wants: BTreeMap::new(),
            tiebreak: BTreeMap::new(),
            providers: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            dial_backoff: BTreeMap::new(),
            frame_backoff: BTreeMap::new(),
        }
    }

    /// The providers of `key` usable for `content_version`/`blake3` right
    /// now: the fed list of exactly that version and hash, minus devices
    /// backing off.
    fn available(
        &self,
        key: &FrameKey,
        content_version: i32,
        blake3: &str,
        now_ms: i64,
    ) -> Vec<ProviderRef> {
        self.providers
            .get(key)
            .and_then(|by_version| by_version.get(&(content_version, blake3.to_string())))
            .map(|ps| {
                ps.iter()
                    .filter(|p| !backing_off(&self.dial_backoff, p.device.as_str(), now_ms))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keep a frame's list under the version and hash it was derived for.
    fn store_list(
        &mut self,
        key: FrameKey,
        content_version: i32,
        blake3: String,
        providers: Vec<ProviderRef>,
    ) {
        let mut ps = providers;
        ps.sort();
        ps.dedup_by(|a, b| a.device == b.device);
        self.providers
            .entry(key)
            .or_default()
            .insert((content_version, blake3), ps);
    }

    fn cancel(&mut self, key: &FrameKey, reason: CancelReason, out: &mut Vec<Command>) {
        if let Some(f) = self.in_flight.remove(key) {
            out.push(Command::Cancel {
                key: key.clone(),
                fetch_id: f.fetch_id,
                reason,
            });
        }
    }

    fn forget_want(&mut self, key: &FrameKey) {
        self.wants.remove(key);
        self.tiebreak.remove(key);
        self.frame_backoff.remove(key);
    }

    /// One input in, the commands it causes out. Every step ends by
    /// reconciling the live provider sets of the fetches in flight and by
    /// scheduling (§7.2):
    ///
    /// - **Start** needs storage, the lane, a free slot and at least one
    ///   provider fed for the want's CURRENT version that is not backing off
    ///   (I4, I5, I11); the frame itself must not be backing off. Order:
    ///   frames wanted longer than [`STARVATION`] first (oldest first), then
    ///   rarest first (fewest available providers), then the per-want random
    ///   draw.
    /// - **A fetch left with no provider** stays in flight — the live run
    ///   waits on its provider set — until another startable frame needs
    ///   its slot and no transfer can still be running for it (see
    ///   [`CancelReason::NoProvider`]); it is then cancelled and stays
    ///   wanted.
    /// - **The lane** is requested only while nothing is in flight and no
    ///   yielded permit is still held; it is released when nothing is in
    ///   flight and either nothing is startable or it was yielded.
    pub fn step(&mut self, now_ms: i64, input: Input) -> Vec<Command> {
        self.now_ms = now_ms;
        let mut out = Vec::new();
        match input {
            Input::NeedSet { project_id, wants } => {
                let incoming: BTreeMap<FrameKey, Want> = wants
                    .into_iter()
                    .filter(|w| w.key.0 == project_id)
                    .map(|w| (w.key.clone(), w))
                    .collect();
                let flying: Vec<FrameKey> = self
                    .in_flight
                    .keys()
                    .filter(|k| k.0 == project_id)
                    .cloned()
                    .collect();
                for k in flying {
                    let f = &self.in_flight[&k];
                    match incoming.get(&k) {
                        None => self.cancel(&k, CancelReason::NotWanted, &mut out),
                        Some(w)
                            if f.content_version != w.content_version || f.blake3 != w.blake3 =>
                        {
                            self.cancel(&k, CancelReason::NewVersion, &mut out)
                        }
                        Some(_) => {}
                    }
                }
                let gone: Vec<FrameKey> = self
                    .wants
                    .keys()
                    .filter(|k| k.0 == project_id && !incoming.contains_key(*k))
                    .cloned()
                    .collect();
                for k in gone {
                    self.forget_want(&k);
                }
                // R1: only the want's own version's list survives, and only
                // for a frame in the set.
                self.providers.retain(|k, by_version| {
                    if k.0 != project_id {
                        return true;
                    }
                    match incoming.get(k) {
                        Some(w) => {
                            // a version number reused after a restore with
                            // another hash does not inherit the old list
                            by_version.retain(|(v, hash), _| {
                                *v == w.content_version && *hash == w.blake3
                            });
                            !by_version.is_empty()
                        }
                        None => false,
                    }
                });
                for (k, w) in incoming {
                    if self
                        .wants
                        .get(&k)
                        .is_some_and(|old| old.content_version != w.content_version)
                    {
                        self.frame_backoff.remove(&k);
                    }
                    if !self.tiebreak.contains_key(&k) {
                        let r = self.rng.next_u64();
                        self.tiebreak.insert(k.clone(), r);
                    }
                    self.wants.insert(k, w);
                }
            }
            Input::Providers {
                key,
                content_version,
                blake3,
                providers,
            } => self.store_list(key, content_version, blake3, providers),
            Input::ProvidersBatch { lists } => {
                for l in lists {
                    self.store_list(l.key, l.content_version, l.blake3, l.providers);
                }
            }
            Input::ProjectGone { project_id } => {
                let flying: Vec<FrameKey> = self
                    .in_flight
                    .keys()
                    .filter(|k| k.0 == project_id)
                    .cloned()
                    .collect();
                for k in flying {
                    self.cancel(&k, CancelReason::ProjectGone, &mut out);
                }
                self.wants.retain(|k, _| k.0 != project_id);
                self.tiebreak.retain(|k, _| k.0 != project_id);
                self.providers.retain(|k, _| k.0 != project_id);
                self.frame_backoff.retain(|k, _| k.0 != project_id);
            }
            Input::Storage { fetching } => {
                self.storage_ok = fetching;
                if !fetching {
                    let flying: Vec<FrameKey> = self.in_flight.keys().cloned().collect();
                    for k in flying {
                        self.cancel(&k, CancelReason::StorageUnavailable, &mut out);
                    }
                }
            }
            Input::Slots(n) => self.slots = n.max(1),
            Input::Lane { admitted: true } => {
                self.lane = true;
                self.yielded = false;
                self.lane_requested = false;
            }
            Input::Lane { admitted: false } => {
                // a yield while the lane is held keeps the permit until the
                // fetches in flight drain; without a permit (a request still
                // pending, or nothing at all) a yield changes nothing
                if self.lane || self.yielded {
                    self.lane = false;
                    self.yielded = true;
                }
            }
            Input::Finished {
                key,
                fetch_id,
                result,
            } => {
                // only the fetch in flight counts: a late result of a
                // cancelled fetch changes nothing
                if self
                    .in_flight
                    .get(&key)
                    .is_some_and(|f| f.fetch_id == fetch_id)
                {
                    self.in_flight.remove(&key);
                    match result {
                        FetchResult::Landed | FetchResult::AwaitingGc => self.forget_want(&key),
                        FetchResult::Failed => {
                            strike(&mut self.frame_backoff, &mut self.rng, key, now_ms)
                        }
                        // a cut at a yield or a stale landing: again at once
                        FetchResult::Cancelled => {}
                    }
                }
            }
            Input::DialFailed { device } | Input::ConnectionClosed { device } => {
                strike(
                    &mut self.dial_backoff,
                    &mut self.rng,
                    device.clone(),
                    now_ms,
                );
                for f in self.in_flight.values_mut() {
                    f.maybe_active.remove(&device);
                }
            }
            Input::ConnectionIdle { device } => {
                for f in self.in_flight.values_mut() {
                    f.maybe_active.remove(&device);
                }
            }
            Input::DialOk { device } => {
                self.dial_backoff.remove(&device);
                for f in self.in_flight.values_mut() {
                    if f.providers.iter().any(|p| p.device == device) {
                        f.maybe_active.insert(device.clone());
                    }
                }
            }
            Input::ClearBackoffs => {
                self.dial_backoff.clear();
                self.frame_backoff.clear();
            }
            Input::Tick => {}
        }
        self.reconcile(now_ms, &mut out);
        self.schedule(now_ms, &mut out);
        out
    }

    /// Bring every in-flight live provider set to what is available now
    /// (a fed change, a back-off ending or starting).
    fn reconcile(&mut self, now_ms: i64, out: &mut Vec<Command>) {
        let changed: Vec<(FrameKey, Vec<ProviderRef>)> = self
            .in_flight
            .iter()
            .filter_map(|(k, f)| {
                let now = self.available(k, f.content_version, &f.blake3, now_ms);
                (now != f.providers).then(|| (k.clone(), now))
            })
            .collect();
        for (k, now) in changed {
            let f = self.in_flight.get_mut(&k).expect("listed above");
            f.maybe_active.extend(now.iter().map(|p| p.device.clone()));
            f.providers = now.clone();
            out.push(Command::UpdateProviders {
                key: k,
                fetch_id: f.fetch_id,
                providers: now,
            });
        }
    }

    /// The wants that could start now, best first (§7.2).
    fn startable(&self, now_ms: i64) -> Vec<FrameKey> {
        if !self.storage_ok {
            return Vec::new();
        }
        let starving = STARVATION.as_millis() as i64;
        let mut candidates: Vec<(bool, i64, usize, u64, FrameKey)> = self
            .wants
            .iter()
            .filter(|(k, _)| !self.in_flight.contains_key(*k))
            .filter(|(k, _)| !backing_off(&self.frame_backoff, *k, now_ms))
            .filter_map(|(k, w)| {
                let n = self
                    .available(k, w.content_version, &w.blake3, now_ms)
                    .len();
                let old = now_ms.saturating_sub(w.since_ms) >= starving;
                (n > 0).then(|| {
                    (
                        !old,
                        if old { w.since_ms } else { 0 },
                        n,
                        self.tiebreak.get(k).copied().unwrap_or(0),
                        k.clone(),
                    )
                })
            })
            .collect();
        candidates.sort();
        candidates.into_iter().map(|c| c.4).collect()
    }

    fn schedule(&mut self, now_ms: i64, out: &mut Vec<Command>) {
        // a yielded lane gives its permit back once its fetches drained
        // (spec §8), before it may queue again
        if self.yielded && self.in_flight.is_empty() {
            self.yielded = false;
            out.push(Command::ReleaseLane);
        }
        let startable = self.startable(now_ms);
        if startable.is_empty() {
            if self.lane && self.in_flight.is_empty() {
                self.lane = false;
                out.push(Command::ReleaseLane);
            }
            return;
        }
        if !self.lane {
            // a yielded lane re-queues only once its units in flight are
            // done and its permit went back
            if !self.lane_requested && !self.yielded && self.in_flight.is_empty() {
                self.lane_requested = true;
                out.push(Command::RequestLane);
            }
            return;
        }
        for key in startable {
            if self.in_flight.len() >= self.slots {
                // Free exactly one slot from dead providerless fetches — all
                // of them at once when the slots were lowered below what is
                // in flight — or start nothing (and cancel nothing).
                let need = self.in_flight.len() + 1 - self.slots;
                let idle: Vec<FrameKey> = self
                    .in_flight
                    .iter()
                    .filter(|(_, f)| f.providers.is_empty() && f.maybe_active.is_empty())
                    .map(|(k, _)| k.clone())
                    .take(need)
                    .collect();
                if idle.len() < need {
                    break;
                }
                for k in idle {
                    self.cancel(&k, CancelReason::NoProvider, out);
                }
            }
            let w = self.wants[&key].clone();
            let providers = self.available(&key, w.content_version, &w.blake3, now_ms);
            let fetch_id = self.next_fetch_id;
            self.next_fetch_id = match self.next_fetch_id.wrapping_add(1) {
                0 => 1,
                n => n,
            };
            self.in_flight.insert(
                key.clone(),
                InFlight {
                    fetch_id,
                    content_version: w.content_version,
                    blake3: w.blake3.clone(),
                    providers: providers.clone(),
                    maybe_active: providers.iter().map(|p| p.device.clone()).collect(),
                },
            );
            out.push(Command::Start {
                key,
                fetch_id,
                content_version: w.content_version,
                blake3: w.blake3,
                byte_size: w.byte_size,
                providers,
            });
        }
    }

    /// The fetch in flight for `key`: `(fetch_id, content_version)`.
    pub fn fetch_of(&self, key: &FrameKey) -> Option<(u64, i32)> {
        self.in_flight
            .get(key)
            .map(|f| (f.fetch_id, f.content_version))
    }

    /// How many frames of `project_id` are still wanted (fed by the latest
    /// need set, not landed since) — a project's "to go".
    pub fn wants_of(&self, project_id: &str) -> usize {
        self.wants
            .range((project_id.to_string(), String::new())..)
            .take_while(|(k, _)| k.0 == project_id)
            .count()
    }

    /// The fetches in flight, `(key, content_version)`, sorted by key.
    pub fn in_flight(&self) -> Vec<(FrameKey, i32)> {
        self.in_flight
            .iter()
            .map(|(k, f)| (k.clone(), f.content_version))
            .collect()
    }

    /// When the executor should step the core with [`Input::Tick`]: the
    /// earliest back-off still running after the latest step, if any.
    pub fn next_wake_ms(&self) -> Option<i64> {
        self.dial_backoff
            .values()
            .chain(self.frame_backoff.values())
            .map(|r| r.until_ms)
            .filter(|u| *u > self.now_ms)
            .min()
    }

    /// The lane is held and not yielded.
    pub fn lane(&self) -> bool {
        self.lane
    }

    /// The lane was yielded and its fetches have not drained yet (the
    /// permit is still held; the simulation's view).
    #[cfg(test)]
    pub(crate) fn lane_yielded(&self) -> bool {
        self.yielded
    }

    /// Whether `device` is backing off at `now_ms` (the simulation's view).
    #[cfg(test)]
    pub(crate) fn device_backing_off(&self, device: &str, now_ms: i64) -> bool {
        backing_off(&self.dial_backoff, device, now_ms)
    }

    /// Whether the frame `key` is backing off at `now_ms`.
    #[cfg(test)]
    pub(crate) fn frame_backing_off(&self, key: &FrameKey, now_ms: i64) -> bool {
        backing_off(&self.frame_backoff, key, now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(u: &str) -> FrameKey {
        ("p1".into(), u.into())
    }
    fn want(u: &str, cv: i32, since: i64) -> Want {
        Want {
            key: key(u),
            content_version: cv,
            blake3: format!("b-{u}-{cv}"),
            byte_size: 10,
            since_ms: since,
        }
    }
    fn prov(d: &str) -> ProviderRef {
        ProviderRef {
            device: d.into(),
            relay_url: None,
        }
    }
    fn provs(u: &str, cv: i32, ds: &[&str]) -> Input {
        Input::Providers {
            key: key(u),
            content_version: cv,
            blake3: format!("b-{u}-{cv}"),
            providers: ds.iter().map(|d| prov(d)).collect(),
        }
    }
    fn need(ws: Vec<Want>) -> Input {
        Input::NeedSet {
            project_id: "p1".into(),
            wants: ws,
        }
    }
    /// The result of the fetch in flight for `u`.
    fn fin(c: &Core, u: &str, result: FetchResult) -> Input {
        let (fetch_id, _) = c.fetch_of(&key(u)).expect("in flight");
        late(u, fetch_id, result)
    }
    /// A result naming fetch `fetch_id` of `u`, in flight or not.
    fn late(u: &str, fetch_id: u64, result: FetchResult) -> Input {
        Input::Finished {
            key: key(u),
            fetch_id,
            result,
        }
    }
    fn starts(cmds: &[Command]) -> Vec<String> {
        cmds.iter()
            .filter_map(|c| match c {
                Command::Start { key, .. } => Some(key.1.clone()),
                _ => None,
            })
            .collect()
    }
    fn started(cmds: &[Command]) -> Vec<(String, i32, Vec<String>)> {
        cmds.iter()
            .filter_map(|c| match c {
                Command::Start {
                    key,
                    content_version,
                    providers,
                    ..
                } => Some((
                    key.1.clone(),
                    *content_version,
                    providers.iter().map(|p| p.device.clone()).collect(),
                )),
                _ => None,
            })
            .collect()
    }
    fn cancelled(cmds: &[Command], u: &str) -> Option<CancelReason> {
        cmds.iter().find_map(|c| match c {
            Command::Cancel { key, reason, .. } if key.1 == u => Some(*reason),
            _ => None,
        })
    }
    fn updated(cmds: &[Command], u: &str) -> Option<Vec<String>> {
        cmds.iter().find_map(|c| match c {
            Command::UpdateProviders { key, providers, .. } if key.1 == u => {
                Some(providers.iter().map(|p| p.device.clone()).collect())
            }
            _ => None,
        })
    }
    fn primed(slots: usize) -> Core {
        let mut c = Core::new(1, slots);
        assert_eq!(
            c.step(
                0,
                need(vec![want("a", 1, 0), want("b", 1, 0), want("c", 1, 0)])
            ),
            vec![]
        );
        c
    }

    /// Task 15: a batch of lists is the same lists fed one by one — the
    /// same frames start, on the same providers — ranked once over all.
    #[test]
    fn a_batch_of_lists_starts_what_the_same_lists_one_by_one_start() {
        let lists = [("a", vec!["X", "Y"]), ("b", vec!["X"])];
        let lane_on_c = || {
            let mut c = primed(3);
            assert_eq!(c.step(0, provs("c", 1, &["Z"])), vec![Command::RequestLane]);
            assert_eq!(
                starts(&c.step(0, Input::Lane { admitted: true })),
                vec!["c"]
            );
            c
        };
        let mut one = lane_on_c();
        let mut seq = Vec::new();
        for (u, ds) in &lists {
            seq.extend(started(&one.step(1, provs(u, 1, ds))));
        }
        let mut all = lane_on_c();
        let batch = started(
            &all.step(
                1,
                Input::ProvidersBatch {
                    lists: lists
                        .iter()
                        .map(|(u, ds)| ProviderList {
                            key: key(u),
                            content_version: 1,
                            blake3: format!("b-{u}-1"),
                            providers: ds.iter().map(|d| prov(d)).collect(),
                        })
                        .collect(),
                },
            ),
        );
        let mut seq_sorted = seq.clone();
        seq_sorted.sort();
        let mut batch_sorted = batch.clone();
        batch_sorted.sort();
        assert_eq!(batch_sorted, seq_sorted);
        assert_eq!(all.in_flight(), one.in_flight());
        let order: Vec<&str> = batch.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(order, vec!["b", "a"], "one ranking over both: rarest first");
    }

    #[test]
    fn providerless_frames_sleep_and_a_provider_wakes_them() {
        let mut c = primed(4);
        assert!(c.step(0, Input::Tick).is_empty());
        let cmds = c.step(1, provs("b", 1, &["X"]));
        assert_eq!(cmds, vec![Command::RequestLane]);
        let cmds = c.step(2, Input::Lane { admitted: true });
        assert_eq!(starts(&cmds), vec!["b".to_string()]);
    }

    #[test]
    fn rarest_first_then_random_and_the_slot_cap_holds() {
        let mut c = primed(1);
        c.step(0, provs("a", 1, &["X", "Y"]));
        c.step(0, provs("b", 1, &["X"]));
        let cmds = c.step(0, Input::Lane { admitted: true });
        assert_eq!(
            starts(&cmds),
            vec!["b".to_string()],
            "the rarer frame goes first"
        );
        assert_eq!(c.in_flight().len(), 1);
    }

    #[test]
    fn a_frame_waiting_an_hour_jumps_the_queue() {
        let mut c = Core::new(1, 1);
        c.step(0, need(vec![want("old", 1, 0), want("rare", 1, 3_500_000)]));
        c.step(0, provs("old", 1, &["X", "Y", "Z"]));
        c.step(0, provs("rare", 1, &["X"]));
        let cmds = c.step(
            STARVATION.as_millis() as i64 + 1,
            Input::Lane { admitted: true },
        );
        assert_eq!(starts(&cmds), vec!["old".to_string()]);
    }

    #[test]
    fn fetch_ids_are_minted_per_start_and_named_by_every_command() {
        let mut c = primed(4);
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, provs("b", 1, &["X"]));
        let cmds = c.step(0, Input::Lane { admitted: true });
        let ids: Vec<u64> = cmds
            .iter()
            .filter_map(|c| match c {
                Command::Start { fetch_id, .. } => Some(*fetch_id),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[1], ids[0] + 1, "consecutive within one core");
        assert_ne!(ids[0], 0);
        assert_ne!(
            Core::new(2, 4).next_fetch_id,
            c.next_fetch_id - 2,
            "another core's ids start elsewhere"
        );
        let cmds = c.step(1, Input::DialFailed { device: "X".into() });
        assert!(cmds.contains(&Command::UpdateProviders {
            key: key("a"),
            fetch_id: ids[0],
            providers: vec![]
        }));
        let cmds = c.step(2, need(vec![want("a", 1, 0)]));
        assert_eq!(
            cmds,
            vec![Command::Cancel {
                key: key("b"),
                fetch_id: ids[1],
                reason: CancelReason::NotWanted
            }]
        );
    }

    #[test]
    fn a_new_version_cancels_the_fetch_in_flight_in_the_same_step() {
        let mut c = primed(4);
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        let cmds = c.step(
            1,
            need(vec![want("a", 2, 0), want("b", 1, 0), want("c", 1, 0)]),
        );
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NewVersion));
        assert!(starts(&cmds).is_empty(), "v1's providers are not v2's");
        assert!(c.in_flight().is_empty());
    }

    #[test]
    fn exclusion_storage_loss_and_a_lost_project_cancel_at_once() {
        let mut c = primed(4);
        for u in ["a", "b"] {
            c.step(0, provs(u, 1, &["X"]));
        }
        c.step(0, Input::Lane { admitted: true });
        assert_eq!(c.in_flight().len(), 2);
        let cmds = c.step(1, need(vec![want("b", 1, 0)]));
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NotWanted));
        let cmds = c.step(2, Input::Storage { fetching: false });
        assert_eq!(
            cancelled(&cmds, "b"),
            Some(CancelReason::StorageUnavailable)
        );
        assert!(
            cmds.contains(&Command::ReleaseLane),
            "an idle lane is released while storage is gone"
        );
        assert!(starts(&c.step(3, Input::Tick)).is_empty());
        c.step(4, Input::Storage { fetching: true });
        let cmds = c.step(
            5,
            Input::ProjectGone {
                project_id: "p1".into(),
            },
        );
        assert!(starts(&cmds).is_empty());
        assert!(c.in_flight().is_empty());
    }

    #[test]
    fn a_failed_dial_backs_off_that_provider_and_sync_now_clears_it() {
        let mut c = primed(4);
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        let cmds = c.step(1, Input::DialFailed { device: "X".into() });
        assert_eq!(updated(&cmds, "a"), Some(vec![]));
        assert!(c.next_wake_ms().is_some());
        let f = fin(&c, "a", FetchResult::Failed);
        c.step(2, f);
        assert!(
            starts(&c.step(3, Input::Tick)).is_empty(),
            "X is backing off"
        );
        // the idle lane was released; clearing the back-offs asks for it again
        assert_eq!(c.step(4, Input::ClearBackoffs), vec![Command::RequestLane]);
        assert_eq!(
            starts(&c.step(5, Input::Lane { admitted: true })),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn a_yielded_lane_takes_no_new_unit_is_released_when_drained_then_requeued() {
        let mut c = primed(4);
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        assert!(c.step(1, Input::Lane { admitted: false }).is_empty());
        assert!(c.step(1, provs("b", 1, &["X"])).is_empty());
        assert!(c.step(2, Input::Tick).is_empty());
        let f = fin(&c, "a", FetchResult::Landed);
        let cmds = c.step(3, f);
        assert_eq!(
            cmds,
            vec![Command::ReleaseLane, Command::RequestLane],
            "the permit goes back first (spec §8), then work re-queues"
        );
        assert!(!c.lane());
    }

    /// Task 12: a project's "to go" is its want set — a landed frame leaves
    /// it at once, a failed one stays; other projects never count.
    #[test]
    fn wants_of_counts_one_project_and_drops_what_landed() {
        let mut c = Core::new(1, 4);
        c.step(0, Input::Storage { fetching: true });
        c.step(0, need(vec![want("a", 1, 0), want("b", 1, 0)]));
        c.step(
            0,
            Input::NeedSet {
                project_id: "p2".into(),
                wants: vec![Want {
                    key: ("p2".into(), "z".into()),
                    ..want("z", 1, 0)
                }],
            },
        );
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, provs("b", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        assert_eq!(
            (c.wants_of("p1"), c.wants_of("p2"), c.wants_of("p")),
            (2, 1, 0)
        );
        let f = fin(&c, "a", FetchResult::Landed);
        c.step(1, f);
        let f = fin(&c, "b", FetchResult::Failed);
        c.step(1, f);
        assert_eq!(c.wants_of("p1"), 1, "landed a leaves, failed b stays");
    }

    #[test]
    fn a_drained_yielded_lane_with_no_work_left_is_only_released() {
        let mut c = primed(4);
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        c.step(1, Input::Lane { admitted: false });
        let f = fin(&c, "a", FetchResult::Landed);
        assert_eq!(c.step(2, f), vec![Command::ReleaseLane]);
        assert!(c.step(3, Input::Tick).is_empty(), "released exactly once");
    }

    #[test]
    fn a_yield_that_races_a_release_releases_nothing_twice() {
        let mut c = primed(4);
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        let f = fin(&c, "a", FetchResult::Landed);
        let cmds = c.step(1, f);
        assert!(cmds.contains(&Command::ReleaseLane));
        // the gate's yield signal for the permit just handed back
        assert!(c.step(2, Input::Lane { admitted: false }).is_empty());
        assert!(!c.lane_yielded());
        // work appears: the lane is simply requested again
        assert_eq!(c.step(3, provs("b", 1, &["X"])), vec![Command::RequestLane]);
    }

    // ---- R1: providers are keyed by version, order-independent ----

    /// The whole command stream of a script, Starts only.
    fn run_script(script: Vec<Input>) -> Vec<(String, i32, Vec<String>)> {
        let mut c = Core::new(9, 4);
        let mut out = Vec::new();
        for (i, input) in script.into_iter().enumerate() {
            let cmds = c.step(i as i64, input);
            if cmds.contains(&Command::RequestLane) {
                out.extend(started(&c.step(i as i64, Input::Lane { admitted: true })));
            }
            out.extend(started(&cmds));
        }
        out
    }

    #[test]
    fn need_set_then_providers_and_providers_then_need_set_reach_the_same_starts() {
        let ns = || need(vec![want("a", 1, 0), want("b", 2, 0)]);
        let pa = || provs("a", 1, &["X", "Y"]);
        let pb = || provs("b", 2, &["Y"]);
        let need_first = run_script(vec![ns(), pa(), pb()]);
        let providers_first = run_script(vec![pa(), pb(), ns()]);
        let mut a = need_first.clone();
        let mut b = providers_first.clone();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        assert_eq!(a.len(), 2, "both frames start in either order: {a:?}");
        assert!(a.contains(&("b".to_string(), 2, vec!["Y".to_string()])));
    }

    #[test]
    fn providers_of_v1_arriving_after_the_need_set_of_v2_start_nothing() {
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 2, 0)]));
        let cmds = c.step(1, provs("a", 1, &["X"]));
        assert!(cmds.is_empty(), "v1 claimants never serve v2: {cmds:?}");
        assert!(c.step(2, Input::Tick).is_empty());
        assert_eq!(c.step(3, provs("a", 2, &["Y"])), vec![Command::RequestLane]);
        let cmds = c.step(4, Input::Lane { admitted: true });
        assert_eq!(
            started(&cmds),
            vec![("a".to_string(), 2, vec!["Y".to_string()])]
        );
    }

    #[test]
    fn a_version_change_clears_the_providers_of_a_frame_not_in_flight() {
        // one slot: `a` flies, `b` waits with a v1 provider
        let mut c = Core::new(1, 1);
        c.step(0, need(vec![want("a", 1, 0), want("b", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        c.step(0, provs("b", 1, &["X", "Y", "Z"]));
        assert_eq!(c.in_flight(), vec![(key("a"), 1)]);
        // `b` moves to v2 before `a` finishes: its v1 claimants are gone
        c.step(1, need(vec![want("a", 1, 0), want("b", 2, 0)]));
        let f = fin(&c, "a", FetchResult::Landed);
        let cmds = c.step(2, f);
        assert!(
            starts(&cmds).is_empty(),
            "b v2 has no provider yet: {cmds:?}"
        );
        let cmds = c.step(3, provs("b", 2, &["Y"]));
        let cmds = if cmds.contains(&Command::RequestLane) {
            c.step(3, Input::Lane { admitted: true })
        } else {
            cmds
        };
        assert_eq!(
            started(&cmds),
            vec![("b".to_string(), 2, vec!["Y".to_string()])]
        );
    }

    #[test]
    fn a_list_for_the_next_version_waits_for_its_need_set() {
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        // the v2 list arrives first: the v1 fetch keeps its own v1 list
        assert!(c.step(1, provs("a", 2, &["Y"])).is_empty());
        // then the need set: v1 is cancelled and v2 starts on Y in the same step
        let cmds = c.step(2, need(vec![want("a", 2, 0)]));
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NewVersion));
        assert_eq!(
            started(&cmds),
            vec![("a".to_string(), 2, vec!["Y".to_string()])]
        );
        // a late v1 list changes nothing
        assert!(c.step(3, provs("a", 1, &["X"])).is_empty());
        assert_eq!(c.in_flight(), vec![(key("a"), 2)]);
    }

    #[test]
    fn a_version_that_goes_down_after_a_restore_starts_on_its_own_list() {
        // spec §4.4: an epoch restore can lower a frame's current version
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 3, 0)]));
        c.step(0, provs("a", 3, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        assert_eq!(c.in_flight(), vec![(key("a"), 3)]);
        // the restored manifest's list arrives before its need set
        assert!(c.step(1, provs("a", 2, &["Y"])).is_empty());
        let cmds = c.step(2, need(vec![want("a", 2, 0)]));
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NewVersion));
        assert_eq!(
            started(&cmds),
            vec![("a".to_string(), 2, vec!["Y".to_string()])]
        );
    }

    // ---- R3: carries from Tasks 12–13 ----

    #[test]
    fn a_fetch_refused_by_every_provider_is_retried_after_its_own_back_off() {
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        let f = fin(&c, "a", FetchResult::Failed);
        let cmds = c.step(10, f);
        assert_eq!(
            cmds,
            vec![Command::ReleaseLane],
            "backing off, nothing else to do"
        );
        assert!(c.step(11, Input::Tick).is_empty());
        let wake = c.next_wake_ms().expect("the frame's back-off is a timer");
        assert!(wake > 10);
        assert_eq!(c.step(wake, Input::Tick), vec![Command::RequestLane]);
        assert_eq!(
            starts(&c.step(wake, Input::Lane { admitted: true })),
            vec!["a".to_string()]
        );
        assert_eq!(c.next_wake_ms(), None, "an expired back-off is no timer");
    }

    #[test]
    fn a_fetch_cut_at_a_yield_is_requeued_at_once() {
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        assert!(c.step(1, Input::Lane { admitted: false }).is_empty());
        let f = fin(&c, "a", FetchResult::Cancelled);
        let cmds = c.step(2, f);
        assert_eq!(
            cmds,
            vec![Command::ReleaseLane, Command::RequestLane],
            "no back-off after a yield"
        );
        assert_eq!(
            starts(&c.step(3, Input::Lane { admitted: true })),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn a_closed_connection_evicts_the_device_from_every_fetch_until_its_back_off_ends() {
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 1, 0), want("b", 1, 0)]));
        c.step(0, provs("a", 1, &["X", "Y"]));
        c.step(0, provs("b", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        assert_eq!(c.in_flight().len(), 2);
        let cmds = c.step(1, Input::ConnectionClosed { device: "X".into() });
        assert_eq!(updated(&cmds, "a"), Some(vec!["Y".to_string()]));
        assert_eq!(updated(&cmds, "b"), Some(vec![]));
        assert_eq!(
            c.in_flight().len(),
            2,
            "a providerless fetch waits in run_live"
        );
        let wake = c.next_wake_ms().expect("X backs off");
        let cmds = c.step(wake, Input::Tick);
        assert_eq!(
            updated(&cmds, "a"),
            Some(vec!["X".to_string(), "Y".to_string()])
        );
        assert_eq!(updated(&cmds, "b"), Some(vec!["X".to_string()]));
    }

    #[test]
    fn a_dead_providerless_fetch_gives_its_slot_to_a_frame_with_a_provider() {
        let mut c = Core::new(1, 1);
        c.step(0, need(vec![want("a", 1, 0), want("b", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        c.step(1, Input::ConnectionClosed { device: "X".into() });
        assert_eq!(c.in_flight(), vec![(key("a"), 1)]);
        let cmds = c.step(2, provs("b", 1, &["Y"]));
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NoProvider));
        assert_eq!(starts(&cmds), vec!["b".to_string()]);
        assert_eq!(c.in_flight(), vec![(key("b"), 1)]);
    }

    #[test]
    fn lowered_slots_never_start_past_the_cap_even_after_a_pre_emption() {
        // sim seed 120: three in flight, slots lowered to 2, one fetch dies —
        // pre-empting it alone left the core at the cap and it started anyway
        let mut c = Core::new(1, 3);
        c.step(
            0,
            need(vec![
                want("a", 1, 0),
                want("b", 1, 0),
                want("c", 1, 0),
                want("d", 1, 0),
            ]),
        );
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, provs("b", 1, &["Y"]));
        c.step(0, provs("c", 1, &["Y"]));
        c.step(0, Input::Lane { admitted: true });
        assert_eq!(c.in_flight().len(), 3);
        c.step(1, Input::Slots(2));
        c.step(1, provs("d", 1, &["Z"]));
        let cmds = c.step(2, Input::ConnectionClosed { device: "X".into() });
        assert!(starts(&cmds).is_empty(), "no room: {cmds:?}");
        assert!(
            !cmds.iter().any(|c| matches!(c, Command::Cancel { .. })),
            "nothing is cancelled for a start that cannot happen: {cmds:?}"
        );
        assert_eq!(c.in_flight().len(), 3);
        // one live fetch ends: now the dead one's slot plus that one make room
        let f = fin(&c, "b", FetchResult::Landed);
        let cmds = c.step(3, f);
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NoProvider));
        assert_eq!(starts(&cmds), vec!["d".to_string()]);
        assert_eq!(c.in_flight().len(), 2);
    }

    #[test]
    fn a_fetch_whose_provider_only_left_presence_keeps_its_slot() {
        // I5: presence never ends an open transfer — X may still be sending.
        let mut c = Core::new(1, 1);
        c.step(0, need(vec![want("a", 1, 0), want("b", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        let cmds = c.step(1, provs("a", 1, &[]));
        assert_eq!(updated(&cmds, "a"), Some(vec![]));
        let cmds = c.step(2, provs("b", 1, &["Y"]));
        assert!(cmds.is_empty(), "a keeps its slot: {cmds:?}");
        // once X's connection closes, nothing can still be flowing
        let cmds = c.step(3, Input::ConnectionClosed { device: "X".into() });
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NoProvider));
        assert_eq!(starts(&cmds), vec!["b".to_string()]);
    }

    #[test]
    fn an_idle_close_frees_the_slot_without_backing_the_device_off() {
        // X refused `a` (ERR_PERMISSION) while connected and dropped its
        // claim; nothing flows from it any more, but only the pool's idle
        // close tells the core so — and that is no failure of X.
        let mut c = Core::new(1, 1);
        c.step(0, need(vec![want("a", 1, 0), want("b", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        c.step(1, provs("a", 1, &[]));
        assert!(
            c.step(2, provs("b", 1, &["X"])).is_empty(),
            "X may still be sending to a"
        );
        let cmds = c.step(3, Input::ConnectionIdle { device: "X".into() });
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NoProvider));
        assert_eq!(
            started(&cmds),
            vec![("b".to_string(), 1, vec!["X".to_string()])],
            "X is not backing off: it serves b at once"
        );
        assert_eq!(c.next_wake_ms(), None, "no back-off for an idle close");
    }

    #[test]
    fn a_late_result_of_a_cancelled_fetch_is_ignored() {
        let mut c = Core::new(1, 1);
        c.step(0, need(vec![want("a", 1, 0), want("b", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        let (first, _) = c.fetch_of(&key("a")).unwrap();
        c.step(1, Input::ConnectionClosed { device: "X".into() });
        let cmds = c.step(2, provs("b", 1, &["Y"]));
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NoProvider));
        // b lands; a restarts at the SAME version once X is usable again
        let f = fin(&c, "b", FetchResult::Landed);
        c.step(3, f);
        assert_eq!(c.step(4, Input::ClearBackoffs), vec![Command::RequestLane]);
        let cmds = c.step(5, Input::Lane { admitted: true });
        assert_eq!(starts(&cmds), vec!["a".to_string()]);
        let (second, _) = c.fetch_of(&key("a")).unwrap();
        assert_ne!(first, second);
        // the first fetch's late results change nothing
        for r in [
            FetchResult::Cancelled,
            FetchResult::Landed,
            FetchResult::AwaitingGc,
            FetchResult::Failed,
        ] {
            let cmds = c.step(6, late("a", first, r));
            assert!(cmds.is_empty(), "{r:?}: {cmds:?}");
            assert_eq!(c.fetch_of(&key("a")), Some((second, 1)), "{r:?}");
            assert!(c.wants.contains_key(&key("a")), "{r:?} forgot the want");
        }
        assert_eq!(c.next_wake_ms(), None, "a late Failed struck nothing");
        let f = fin(&c, "a", FetchResult::Landed);
        assert_eq!(c.step(7, f), vec![Command::ReleaseLane]);
        assert!(c.in_flight().is_empty());
    }

    #[test]
    fn a_re_dialled_device_counts_as_sending_again() {
        // fix round 2, A: an idle close then a new dial — X is sending again.
        // No live-set change sits between the dial and the presence drop, so
        // nothing but DialOk can restore X (in the review's variant a close
        // of a second provider Y re-added X through the live-set update).
        let mut c = Core::new(1, 1);
        c.step(0, need(vec![want("a", 1, 0), want("b", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        c.step(1, Input::ConnectionIdle { device: "X".into() });
        assert!(c.step(2, Input::DialOk { device: "X".into() }).is_empty());
        // presence drops X from a's list: X's transfer may still be running
        let cmds = c.step(3, provs("a", 1, &[]));
        assert_eq!(updated(&cmds, "a"), Some(vec![]));
        let cmds = c.step(4, provs("b", 1, &["Z"]));
        assert_eq!(
            cancelled(&cmds, "a"),
            None,
            "a running transfer is never cut: {cmds:?}"
        );
        assert!(starts(&cmds).is_empty());
        assert_eq!(c.in_flight(), vec![(key("a"), 1)]);
    }

    #[test]
    fn the_reviewed_re_dial_scenario_keeps_the_running_transfer() {
        // the review's exact sequence: live [X, Y], idle close X, DialOk X,
        // close Y, presence drops X, another frame startable, slots full
        let mut c = Core::new(1, 1);
        c.step(0, need(vec![want("a", 1, 0), want("b", 1, 0)]));
        c.step(0, provs("a", 1, &["X", "Y"]));
        c.step(0, Input::Lane { admitted: true });
        c.step(1, Input::ConnectionIdle { device: "X".into() });
        c.step(2, Input::DialOk { device: "X".into() });
        c.step(3, Input::ConnectionClosed { device: "Y".into() });
        c.step(4, provs("a", 1, &[]));
        let cmds = c.step(5, provs("b", 1, &["Z"]));
        assert_eq!(cancelled(&cmds, "a"), None, "{cmds:?}");
        assert_eq!(c.in_flight(), vec![(key("a"), 1)]);
    }

    #[test]
    fn a_late_yield_while_a_request_is_pending_never_requests_twice() {
        // fix round 2, B: two requests, one release would leak a permit
        let mut c = primed(4);
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        let f = fin(&c, "a", FetchResult::Landed);
        assert_eq!(c.step(1, f), vec![Command::ReleaseLane]);
        assert_eq!(c.step(2, provs("b", 1, &["X"])), vec![Command::RequestLane]);
        // the yield signal of the permit handed back arrives late
        assert!(c.step(3, Input::Lane { admitted: false }).is_empty());
        assert!(c.step(4, Input::Tick).is_empty(), "still one request");
        assert!(!c.lane_yielded() && !c.lane());
        assert_eq!(
            starts(&c.step(5, Input::Lane { admitted: true })),
            vec!["b".to_string()]
        );
    }

    #[test]
    fn a_reused_version_number_with_a_new_hash_drops_the_old_list() {
        // fix round 2, minor 3: an epoch restore reuses v2 for other content
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 2, 0)]));
        c.step(0, provs("a", 2, &["X"])); // derived for b-a-2
        assert_eq!(
            starts(&c.step(0, Input::Lane { admitted: true })),
            vec!["a".to_string()]
        );
        let mut w2 = want("a", 2, 0);
        w2.blake3 = "b-a-2-restored".into();
        let cmds = c.step(1, need(vec![w2.clone()]));
        assert_eq!(cancelled(&cmds, "a"), Some(CancelReason::NewVersion));
        assert!(
            starts(&cmds).is_empty(),
            "the old v2 list never serves the new v2: {cmds:?}"
        );
        assert!(
            cmds.contains(&Command::ReleaseLane),
            "nothing left to fetch"
        );
        assert!(c.providers.get(&key("a")).is_none(), "the old list is gone");
        // the new content's list starts it; a late list of the old content
        // arriving after it never overwrites it
        let cmds = c.step(
            2,
            Input::Providers {
                key: key("a"),
                content_version: 2,
                blake3: w2.blake3.clone(),
                providers: vec![prov("Y")],
            },
        );
        assert_eq!(cmds, vec![Command::RequestLane]);
        assert_eq!(
            started(&c.step(2, Input::Lane { admitted: true })),
            vec![("a".to_string(), 2, vec!["Y".to_string()])]
        );
        assert!(c.step(3, provs("a", 2, &["X"])).is_empty());
        let (id, _) = c.fetch_of(&key("a")).unwrap();
        c.step(4, Input::ConnectionClosed { device: "Y".into() });
        let cmds = c.step(4, Input::ClearBackoffs);
        assert_eq!(
            updated(&cmds, "a"),
            Some(vec!["Y".to_string()]),
            "the live set is still the new content's list"
        );
        assert_eq!(c.fetch_of(&key("a")).map(|f| f.0), Some(id));
    }

    #[test]
    fn a_dial_back_off_grows_and_a_good_dial_resets_it() {
        let mut c = Core::new(3, 4);
        let mut last = 0i64;
        for i in 0..8 {
            c.step(i, Input::DialFailed { device: "X".into() });
            let wake = c.next_wake_ms().expect("backing off");
            assert!(wake > i);
            last = last.max(wake - i);
        }
        assert!(last > 1_000, "the envelope grew past the 1 s base: {last}");
        c.step(100, Input::DialOk { device: "X".into() });
        assert_eq!(c.next_wake_ms(), None);
    }

    #[test]
    fn the_same_seed_gives_the_same_commands() {
        let script = || {
            vec![
                need(vec![
                    want("a", 1, 0),
                    want("b", 1, 0),
                    want("c", 1, 0),
                    want("d", 1, 0),
                ]),
                provs("a", 1, &["X"]),
                provs("b", 1, &["X"]),
                provs("c", 1, &["X"]),
                provs("d", 1, &["X"]),
                Input::Lane { admitted: true },
            ]
        };
        let run = |seed| {
            let mut c = Core::new(seed, 2);
            script()
                .into_iter()
                .flat_map(|i| c.step(0, i))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(5), run(5));
    }
}
