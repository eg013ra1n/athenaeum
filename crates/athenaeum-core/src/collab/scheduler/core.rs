//! The receive-side scheduler as a pure, deterministic core (spec §7.1):
//! hub events, disk events, fetch results and timers in; fetch, cancel and
//! lane commands out. The executor (`api::collab_live`, Task 15) performs
//! the commands and re-checks every precondition against the database; the
//! landing fence stays a DB conditional (I1). BTreeMaps and one seeded RNG
//! make every run reproducible (§12). No I/O and no logging happen here —
//! the executor logs the commands it performs.

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
    /// The fetch has no provider left and none can still be transferring
    /// (every device it was given has since failed a dial or closed its
    /// connection), while another frame with a provider needs its slot. The
    /// frame stays wanted and restarts when a provider appears; its partial
    /// bytes stay in the store for the resume.
    NoProvider,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// The project's whole need set (a replacement). A fetch in flight for a
    /// frame that left it is cancelled (`NotWanted`), one whose version or
    /// hash moved is cancelled (`NewVersion`) — both in this step. Provider
    /// lists survive only for frames in the set at the version they were
    /// derived for, so the executor feeds a frame's list whenever it
    /// (re-)enters the set, before or after this input — the order does not
    /// matter (R1).
    NeedSet {
        project_id: String,
        wants: Vec<Want>,
    },
    /// A frame's candidates (I4, I5), derived for `content_version` — the
    /// manifest's current version when the executor derived them. A list
    /// for an older version than the frame's want (or than a list already
    /// known) is ignored; a version change of the want clears the list.
    Providers {
        key: FrameKey,
        content_version: i32,
        providers: Vec<ProviderRef>,
    },
    ProjectGone {
        project_id: String,
    },
    Storage {
        fetching: bool,
    },
    /// `collab.max_receive_streams`.
    Slots(usize),
    /// The collab ReceiveGate permit is held (`true`) / was yielded (`false`).
    Lane {
        admitted: bool,
    },
    /// A fetch the core started ended. The executor drops the late result
    /// of a fetch the core cancelled (a Start of the same frame may already
    /// follow it); a result naming an older version than the fetch in
    /// flight never touches that fetch.
    Finished {
        key: FrameKey,
        content_version: i32,
        result: FetchResult,
    },
    /// A dial failed or a pooled connection closed (§7.3): the device backs
    /// off (1 s → 60 s, full jitter) and leaves every live provider set
    /// until then (I5).
    DialFailed {
        device: String,
    },
    ConnectionClosed {
        device: String,
    },
    /// A dial succeeded: the device's back-off ends.
    DialOk {
        device: String,
    },
    /// Sync now (L10): every back-off ends.
    ClearBackoffs,
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start {
        key: FrameKey,
        content_version: i32,
        blake3: String,
        byte_size: i64,
        providers: Vec<ProviderRef>,
    },
    /// The live provider set of a fetch in flight.
    UpdateProviders {
        key: FrameKey,
        providers: Vec<ProviderRef>,
    },
    Cancel {
        key: FrameKey,
        reason: CancelReason,
    },
    RequestLane,
    ReleaseLane,
}

/// A frame's candidates as last fed, with the version they were derived for
/// (R1: providers are keyed by version, never reused across one).
#[derive(Debug, Clone)]
struct Fed {
    content_version: i32,
    providers: Vec<ProviderRef>,
}

#[derive(Debug, Clone)]
struct InFlight {
    content_version: i32,
    blake3: String,
    /// The live provider set as last commanded.
    providers: Vec<ProviderRef>,
    /// Every device this fetch was given that may still be sending to it:
    /// a device leaves only through a failed dial or a closed connection
    /// (I5 — a presence change never ends an open transfer).
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
    /// A `RequestLane` is outstanding (answered by `Lane`).
    lane_requested: bool,
    storage_ok: bool,
    /// The `now_ms` of the latest step (for [`Core::next_wake_ms`]).
    now_ms: i64,
    wants: BTreeMap<FrameKey, Want>,
    /// The per-want random draw behind "rarest first, then random" (§7.2).
    tiebreak: BTreeMap<FrameKey, u64>,
    providers: BTreeMap<FrameKey, Fed>,
    in_flight: BTreeMap<FrameKey, InFlight>,
    /// Per provider device: a failed dial or a closed connection (§7.3).
    dial_backoff: BTreeMap<String, Retry>,
    /// Per frame: a failed fetch (incl. refused by every provider).
    frame_backoff: BTreeMap<FrameKey, Retry>,
}

impl Core {
    pub fn new(seed: u64, slots: usize) -> Self {
        Self {
            rng: SplitMix64(seed),
            slots: slots.max(1),
            lane: false,
            lane_requested: false,
            storage_ok: true,
            now_ms: i64::MIN,
            wants: BTreeMap::new(),
            tiebreak: BTreeMap::new(),
            providers: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            dial_backoff: BTreeMap::new(),
            frame_backoff: BTreeMap::new(),
        }
    }

    /// The providers of `key` usable for `content_version` right now: the
    /// fed list of exactly that version, minus devices backing off.
    fn available(&self, key: &FrameKey, content_version: i32, now_ms: i64) -> Vec<ProviderRef> {
        match self.providers.get(key) {
            Some(fed) if fed.content_version == content_version => fed
                .providers
                .iter()
                .filter(|p| !backing_off(&self.dial_backoff, p.device.as_str(), now_ms))
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn cancel(&mut self, key: &FrameKey, reason: CancelReason, out: &mut Vec<Command>) {
        if self.in_flight.remove(key).is_some() {
            out.push(Command::Cancel {
                key: key.clone(),
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
    ///   its slot and no transfer can still be running for it (every device
    ///   it was given has since failed a dial or closed its connection); it
    ///   is then cancelled with [`CancelReason::NoProvider`] and stays
    ///   wanted.
    /// - **The lane** is requested only while nothing is in flight (a
    ///   yielded lane re-queues once its units are done) and released when
    ///   nothing is in flight and nothing is startable.
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
                // R1: a list survives only for a wanted frame at the version
                // it was derived for — never across a version change, in
                // flight or not; a frame entering the need set later gets a
                // fresh list from the executor.
                self.providers.retain(|k, fed| {
                    k.0 != project_id
                        || incoming
                            .get(k)
                            .is_some_and(|w| w.content_version == fed.content_version)
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
                providers,
            } => {
                let older_than_want = self
                    .wants
                    .get(&key)
                    .is_some_and(|w| content_version < w.content_version);
                let older_than_known = self
                    .providers
                    .get(&key)
                    .is_some_and(|f| content_version < f.content_version);
                if !older_than_want && !older_than_known {
                    let mut ps = providers;
                    ps.sort();
                    ps.dedup_by(|a, b| a.device == b.device);
                    self.providers.insert(
                        key,
                        Fed {
                            content_version,
                            providers: ps,
                        },
                    );
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
            Input::Lane { admitted } => {
                self.lane = admitted;
                self.lane_requested = false;
            }
            Input::Finished {
                key,
                content_version,
                result,
            } => {
                // A result of an older fetch never touches a newer one.
                if self
                    .in_flight
                    .get(&key)
                    .is_some_and(|f| f.content_version == content_version)
                {
                    self.in_flight.remove(&key);
                }
                let current = self
                    .wants
                    .get(&key)
                    .is_some_and(|w| w.content_version == content_version);
                if current {
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
            Input::DialOk { device } => {
                self.dial_backoff.remove(&device);
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
    /// (a fed change, a back-off ending or starting, a version moving on).
    fn reconcile(&mut self, now_ms: i64, out: &mut Vec<Command>) {
        let changed: Vec<(FrameKey, Vec<ProviderRef>)> = self
            .in_flight
            .iter()
            .filter_map(|(k, f)| {
                let now = self.available(k, f.content_version, now_ms);
                (now != f.providers).then(|| (k.clone(), now))
            })
            .collect();
        for (k, now) in changed {
            let f = self.in_flight.get_mut(&k).expect("listed above");
            f.maybe_active.extend(now.iter().map(|p| p.device.clone()));
            f.providers = now.clone();
            out.push(Command::UpdateProviders {
                key: k,
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
                let n = self.available(k, w.content_version, now_ms).len();
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
        let startable = self.startable(now_ms);
        if startable.is_empty() {
            if self.lane && self.in_flight.is_empty() {
                self.lane = false;
                out.push(Command::ReleaseLane);
            }
            return;
        }
        if !self.lane {
            // a yielded lane re-queues only once its units in flight are done
            if !self.lane_requested && self.in_flight.is_empty() {
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
            let providers = self.available(&key, w.content_version, now_ms);
            self.in_flight.insert(
                key.clone(),
                InFlight {
                    content_version: w.content_version,
                    blake3: w.blake3.clone(),
                    providers: providers.clone(),
                    maybe_active: providers.iter().map(|p| p.device.clone()).collect(),
                },
            );
            out.push(Command::Start {
                key,
                content_version: w.content_version,
                blake3: w.blake3,
                byte_size: w.byte_size,
                providers,
            });
        }
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
            providers: ds.iter().map(|d| prov(d)).collect(),
        }
    }
    fn need(ws: Vec<Want>) -> Input {
        Input::NeedSet {
            project_id: "p1".into(),
            wants: ws,
        }
    }
    fn finished(u: &str, cv: i32, result: FetchResult) -> Input {
        Input::Finished {
            key: key(u),
            content_version: cv,
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
    fn a_new_version_cancels_the_fetch_in_flight_in_the_same_step() {
        let mut c = primed(4);
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        let cmds = c.step(
            1,
            need(vec![want("a", 2, 0), want("b", 1, 0), want("c", 1, 0)]),
        );
        assert!(cmds.contains(&Command::Cancel {
            key: key("a"),
            reason: CancelReason::NewVersion
        }));
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
        assert!(cmds.contains(&Command::Cancel {
            key: key("a"),
            reason: CancelReason::NotWanted
        }));
        let cmds = c.step(2, Input::Storage { fetching: false });
        assert!(cmds.contains(&Command::Cancel {
            key: key("b"),
            reason: CancelReason::StorageUnavailable
        }));
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
        assert!(cmds.contains(&Command::UpdateProviders {
            key: key("a"),
            providers: vec![]
        }));
        assert!(c.next_wake_ms().is_some());
        c.step(2, finished("a", 1, FetchResult::Failed));
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
    fn a_yielded_lane_takes_no_new_unit_and_is_released_when_idle() {
        let mut c = primed(4);
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        c.step(1, Input::Lane { admitted: false });
        c.step(1, provs("b", 1, &["X"]));
        assert!(starts(&c.step(2, Input::Tick)).is_empty());
        let cmds = c.step(3, finished("a", 1, FetchResult::Landed));
        assert!(
            cmds.contains(&Command::RequestLane),
            "work remains: ask for the lane again"
        );
        assert!(!c.lane());
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
        let cmds = c.step(2, finished("a", 1, FetchResult::Landed));
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
    fn providers_ahead_of_the_need_set_leave_the_old_fetch_providerless_until_it_is_cancelled() {
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        // the v2 list arrives first: the v1 fetch keeps no v2 claimant
        let cmds = c.step(1, provs("a", 2, &["Y"]));
        assert_eq!(
            cmds,
            vec![Command::UpdateProviders {
                key: key("a"),
                providers: vec![]
            }]
        );
        // then the need set: v1 is cancelled and v2 starts on Y in the same step
        let cmds = c.step(2, need(vec![want("a", 2, 0)]));
        assert!(cmds.contains(&Command::Cancel {
            key: key("a"),
            reason: CancelReason::NewVersion
        }));
        assert_eq!(
            started(&cmds),
            vec![("a".to_string(), 2, vec!["Y".to_string()])]
        );
        // a late v1 list changes nothing
        assert!(c.step(3, provs("a", 1, &["X"])).is_empty());
        assert_eq!(c.in_flight(), vec![(key("a"), 2)]);
    }

    // ---- R3: carries from Tasks 12–13 ----

    #[test]
    fn a_fetch_refused_by_every_provider_is_retried_after_its_own_back_off() {
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        let cmds = c.step(10, finished("a", 1, FetchResult::Failed));
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
        let cmds = c.step(2, finished("a", 1, FetchResult::Cancelled));
        assert_eq!(
            cmds,
            vec![Command::RequestLane],
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
        assert!(cmds.contains(&Command::UpdateProviders {
            key: key("a"),
            providers: vec![prov("Y")]
        }));
        assert!(cmds.contains(&Command::UpdateProviders {
            key: key("b"),
            providers: vec![]
        }));
        assert_eq!(
            c.in_flight().len(),
            2,
            "a providerless fetch waits in run_live"
        );
        let wake = c.next_wake_ms().expect("X backs off");
        let cmds = c.step(wake, Input::Tick);
        assert!(cmds.contains(&Command::UpdateProviders {
            key: key("a"),
            providers: vec![prov("X"), prov("Y")]
        }));
        assert!(cmds.contains(&Command::UpdateProviders {
            key: key("b"),
            providers: vec![prov("X")]
        }));
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
        assert!(cmds.contains(&Command::Cancel {
            key: key("a"),
            reason: CancelReason::NoProvider
        }));
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
        let cmds = c.step(3, finished("b", 1, FetchResult::Landed));
        assert!(cmds.contains(&Command::Cancel {
            key: key("a"),
            reason: CancelReason::NoProvider
        }));
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
        assert_eq!(
            cmds,
            vec![Command::UpdateProviders {
                key: key("a"),
                providers: vec![]
            }]
        );
        let cmds = c.step(2, provs("b", 1, &["Y"]));
        assert!(cmds.is_empty(), "a keeps its slot: {cmds:?}");
        // once X's connection closes, nothing can still be flowing
        let cmds = c.step(3, Input::ConnectionClosed { device: "X".into() });
        assert!(cmds.contains(&Command::Cancel {
            key: key("a"),
            reason: CancelReason::NoProvider
        }));
        assert_eq!(starts(&cmds), vec!["b".to_string()]);
    }

    #[test]
    fn a_stale_result_never_touches_the_newer_fetch() {
        let mut c = Core::new(1, 4);
        c.step(0, need(vec![want("a", 1, 0)]));
        c.step(0, provs("a", 1, &["X"]));
        c.step(0, Input::Lane { admitted: true });
        c.step(1, provs("a", 2, &["Y"]));
        c.step(1, need(vec![want("a", 2, 0)]));
        assert_eq!(c.in_flight(), vec![(key("a"), 2)]);
        let cmds = c.step(2, finished("a", 1, FetchResult::Failed));
        assert!(cmds.is_empty(), "{cmds:?}");
        assert_eq!(c.in_flight(), vec![(key("a"), 2)]);
        assert_eq!(c.next_wake_ms(), None, "v1's failure backs off nothing");
        c.step(3, finished("a", 1, FetchResult::Landed));
        assert_eq!(c.in_flight(), vec![(key("a"), 2)]);
        c.step(4, finished("a", 2, FetchResult::Landed));
        assert!(c.in_flight().is_empty());
        assert!(!c.lane(), "the idle lane was released");
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
