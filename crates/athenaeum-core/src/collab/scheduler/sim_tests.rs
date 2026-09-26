//! Seeded randomized interleavings of hub events (`project` and `holders`,
//! the need set and the provider lists fed in BOTH orders by a seeded coin),
//! presence, membership, disk events, fetch results, lane yields, back-offs
//! and crashes, with the live-exchange invariants asserted after every core
//! step and after every event (spec §12, I1–I11). The world derives
//! providers with the production `holders::providers` over the production
//! `PresenceBook`; the feed-cursor test drives the production
//! `cursor::{step, plan_hello, plan_versions}`; the serve test the
//! production `serve::decide`.
//!
//! Reproduce one failing seed with
//! `COLLAB_SIM_SEED=<n> cargo test -p athenaeum-core --lib collab::scheduler::sim_tests -- --nocapture`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::core::*;
use crate::collab::live::holders::{providers as derive_providers, FrameRef, ProjectHolders};
use crate::collab::live::presence::PresenceBook;
use crate::collab::live::wire::{HolderDeltaWire, PresenceChange, PresenceEvent};
use crate::geometry::ransac::SplitMix64;

const PID: &str = "p1";
const ME: &str = "ME";
const PEERS: [&str; 4] = ["A", "B", "C", "D"];
const FRAMES: usize = 10;
const STEPS: usize = 400;
const SEEDS: u64 = 300;

#[derive(Clone, Debug)]
struct HubFrame {
    cv: i32,
    published: bool,
    accepted: bool,
}

/// The frame's local state as the executor's DB holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Local {
    Wanted,
    Held(i32),
    Quarantined,
    NotKept,
    Idle,
    /// Parked over a dead store entry until the GC runs (`awaiting_gc`).
    Parked,
}

/// A fetch as the executor sees it.
#[derive(Clone, Debug)]
struct Fetch {
    id: u64,
    cv: i32,
    providers: BTreeSet<String>,
    /// Devices given to it that may still be sending (minus those whose
    /// dial failed or whose connection closed since) — recomputed here from
    /// the commands and the inputs, independently of the core.
    maybe_active: BTreeSet<String>,
    /// The last device to leave `maybe_active` left by an idle close.
    idle_cleared: bool,
}

/// How often each path ran, summed over the seeds: the simulation must
/// exercise every one of them, not idle past them.
#[derive(Debug, Default, Clone, Copy)]
struct Coverage {
    busy_steps: usize,
    starts: usize,
    landed: usize,
    failed: usize,
    starving_starts: usize,
    provider_updates: usize,
    cancel_new_version: usize,
    cancel_not_wanted: usize,
    cancel_project_gone: usize,
    cancel_storage: usize,
    cancel_no_provider: usize,
    yields: usize,
    lane_releases: usize,
    /// a Start on a list fed BEFORE the need set that introduced the want
    starts_providers_first: usize,
    slots_below_in_flight: usize,
    /// a late result of a cancelled fetch while the frame was in flight again
    late_ignored: usize,
    idle_freed_slot: usize,
    version_down: usize,
}

impl Coverage {
    fn add(&mut self, o: &Coverage) {
        self.busy_steps += o.busy_steps;
        self.starts += o.starts;
        self.landed += o.landed;
        self.failed += o.failed;
        self.starving_starts += o.starving_starts;
        self.provider_updates += o.provider_updates;
        self.cancel_new_version += o.cancel_new_version;
        self.cancel_not_wanted += o.cancel_not_wanted;
        self.cancel_project_gone += o.cancel_project_gone;
        self.cancel_storage += o.cancel_storage;
        self.cancel_no_provider += o.cancel_no_provider;
        self.yields += o.yields;
        self.lane_releases += o.lane_releases;
        self.starts_providers_first += o.starts_providers_first;
        self.slots_below_in_flight += o.slots_below_in_flight;
        self.late_ignored += o.late_ignored;
        self.idle_freed_slot += o.idle_freed_slot;
        self.version_down += o.version_down;
    }
}

/// Work the executor does after the core's commands were checked.
enum FollowUp {
    GrantLane,
    /// The local shortcut at Start (adopt by hash, identical landed file):
    /// `(frame, fetch_id, version)`.
    LocalLanded(String, u64, i32),
    /// A waiting item cut at a yield.
    YieldCut(String, u64, i32),
}

struct World {
    rng: SplitMix64,
    now: i64,
    frames: BTreeMap<String, HubFrame>,
    holders: ProjectHolders,
    presence: PresenceBook,
    members: HashSet<String>,
    local: BTreeMap<String, Local>,
    wanted_since: BTreeMap<String, i64>,
    storage_ok: bool,
    project_live: bool,
    slots: usize,
    core: Core,
    // ---- the executor's side ----
    flying: BTreeMap<String, Fetch>,
    lane_admitted: bool,
    /// The lane was yielded and its fetches have not drained yet.
    lane_yielded: bool,
    lane_pending: bool,
    personal: bool,
    /// Results of cancelled fetches still on their way `(frame, fetch_id,
    /// version, result)` — the executor forwards them late.
    late: Vec<(String, u64, i32, FetchResult)>,
    // ---- a mirror of what the core was fed (core-relative checks) ----
    fed_need: BTreeMap<String, Want>,
    /// frame → version → (feed sequence number, list)
    fed_providers: BTreeMap<String, BTreeMap<i32, (u64, Vec<ProviderRef>)>>,
    /// frame → (version, feed sequence number when that want first came)
    need_seq: BTreeMap<String, (i32, u64)>,
    feed_seq: u64,
    fed_storage: bool,
    followups: Vec<FollowUp>,
    /// `COLLAB_SIM_TRACE=1`: print every input and its commands.
    trace: bool,
    cov: Coverage,
}

fn key(u: &str) -> FrameKey {
    (PID.to_string(), u.to_string())
}

fn seq_of(u: &str) -> i32 {
    u[1..].parse::<i32>().expect("frame names are f<nn>") + 1
}

fn blake(u: &str, cv: i32) -> String {
    format!("b-{u}-{cv}")
}

impl World {
    fn new(seed: u64) -> World {
        let mut w = World {
            rng: SplitMix64(seed ^ 0x5eed_c011_ab00),
            now: 0,
            frames: (0..FRAMES)
                .map(|i| {
                    (
                        format!("f{i:02}"),
                        HubFrame {
                            cv: 1,
                            published: true,
                            accepted: true,
                        },
                    )
                })
                .collect(),
            holders: ProjectHolders::default(),
            presence: PresenceBook::default(),
            members: std::iter::once(ME)
                .chain(PEERS)
                .map(str::to_string)
                .collect(),
            local: (0..FRAMES)
                .map(|i| (format!("f{i:02}"), Local::Wanted))
                .collect(),
            wanted_since: (0..FRAMES).map(|i| (format!("f{i:02}"), 0)).collect(),
            storage_ok: true,
            project_live: true,
            slots: 3,
            core: Core::new(seed, 3),
            flying: BTreeMap::new(),
            lane_admitted: false,
            lane_yielded: false,
            lane_pending: false,
            personal: false,
            late: Vec::new(),
            fed_need: BTreeMap::new(),
            fed_providers: BTreeMap::new(),
            need_seq: BTreeMap::new(),
            feed_seq: 0,
            fed_storage: true,
            followups: Vec::new(),
            trace: std::env::var_os("COLLAB_SIM_TRACE").is_some(),
            cov: Coverage::default(),
        };
        w.presence.apply_hello(PID, &[]);
        // the publisher's first holdings: every frame v1 on one peer
        for u in w.frames.keys().cloned().collect::<Vec<_>>() {
            let d = PEERS[w.rng.below(PEERS.len())].to_string();
            w.holders.apply_delta(&HolderDeltaWire {
                device: d,
                add: vec![(seq_of(&u), 1)],
                rm: vec![],
            });
        }
        w
    }

    // ---------------------------------------------------------------- model

    fn in_replication_set(&self, u: &str) -> bool {
        let f = &self.frames[u];
        self.project_live && f.published && f.accepted
    }

    /// What the executor computes from the DB: the need set (I8, T15 M-d:
    /// `local_state == wanted` only).
    fn need_set(&self) -> Vec<Want> {
        if !self.project_live {
            return vec![];
        }
        self.frames
            .iter()
            .filter(|(u, _)| self.in_replication_set(u) && self.local[*u] == Local::Wanted)
            .map(|(u, f)| Want {
                key: key(u),
                content_version: f.cv,
                blake3: blake(u, f.cv),
                byte_size: 10,
                since_ms: self.wanted_since[u],
            })
            .collect()
    }

    /// Providers of `u` at version `cv` with the PRODUCTION derivation.
    fn providers_at(&self, u: &str, cv: i32) -> Vec<ProviderRef> {
        let pubs: HashSet<String> = HashSet::new();
        let f = FrameRef {
            project_id: PID,
            frame_seq: seq_of(u),
            content_version: cv,
            publisher_devices: &pubs,
        };
        derive_providers(&self.holders, &self.presence, &self.members, ME, &f)
            .into_iter()
            .map(|p| ProviderRef {
                device: p.device,
                relay_url: p.relay_url,
            })
            .collect()
    }

    fn providers_of(&self, u: &str) -> Vec<ProviderRef> {
        self.providers_at(u, self.frames[u].cv)
    }

    fn set_wanted(&mut self, u: &str) {
        self.local.insert(u.to_string(), Local::Wanted);
        self.wanted_since.insert(u.to_string(), self.now);
    }

    // ------------------------------------------------------------- feeding

    fn feed_need(&mut self) {
        let wants = self.need_set();
        self.feed(Input::NeedSet {
            project_id: PID.into(),
            wants,
        });
    }

    fn feed_providers(&mut self, u: &str) {
        let providers = self.providers_of(u);
        let cv = self.frames[u].cv;
        self.feed(Input::Providers {
            key: key(u),
            content_version: cv,
            providers,
        });
    }

    /// The executor's full refresh after an event that may move the need set
    /// or any provider list, in a seeded order (R1: the core must reach the
    /// same state whichever comes first).
    fn resync_all(&mut self) {
        if !self.project_live {
            return;
        }
        let need_first = self.rng.below(2) == 0;
        if need_first {
            self.feed_need();
        }
        for u in self.frames.keys().cloned().collect::<Vec<_>>() {
            self.feed_providers(&u);
        }
        if !need_first {
            self.feed_need();
        }
    }

    /// Mirror an input into the "what the core was told" record, by the
    /// rules the core documents (R1), then step the core and check.
    fn feed(&mut self, input: Input) {
        self.feed_seq += 1;
        let seq = self.feed_seq;
        match &input {
            Input::NeedSet { wants, .. } => {
                let incoming: BTreeMap<String, Want> =
                    wants.iter().map(|w| (w.key.1.clone(), w.clone())).collect();
                self.fed_providers
                    .retain(|u, by_version| match incoming.get(u) {
                        Some(w) => {
                            by_version.retain(|v, _| *v == w.content_version);
                            !by_version.is_empty()
                        }
                        None => false,
                    });
                self.need_seq
                    .retain(|u, (cv, _)| incoming.get(u).is_some_and(|w| w.content_version == *cv));
                for (u, w) in &incoming {
                    self.need_seq
                        .entry(u.clone())
                        .or_insert((w.content_version, seq));
                }
                self.fed_need = incoming;
            }
            Input::Providers {
                key,
                content_version,
                providers,
            } => {
                let mut ps = providers.clone();
                ps.sort();
                ps.dedup_by(|a, b| a.device == b.device);
                self.fed_providers
                    .entry(key.1.clone())
                    .or_default()
                    .insert(*content_version, (seq, ps));
            }
            Input::ProjectGone { .. } => {
                self.fed_need.clear();
                self.fed_providers.clear();
                self.need_seq.clear();
            }
            Input::Storage { fetching } => self.fed_storage = *fetching,
            Input::Lane { admitted } => {
                self.lane_yielded = !*admitted && (self.lane_admitted || self.lane_yielded);
                self.lane_admitted = *admitted;
            }
            Input::Finished {
                key,
                fetch_id,
                result,
            } => {
                let u = &key.1;
                match self.flying.get(u) {
                    Some(f) if f.id == *fetch_id => {
                        self.flying.remove(u);
                        if matches!(result, FetchResult::Landed | FetchResult::AwaitingGc) {
                            self.fed_need.remove(u);
                            self.need_seq.remove(u);
                        }
                    }
                    // a late result while the frame runs again: the
                    // dangerous case the fetch id exists for
                    Some(_) => self.cov.late_ignored += 1,
                    None => {}
                }
            }
            Input::DialFailed { device } | Input::ConnectionClosed { device } => {
                for f in self.flying.values_mut() {
                    if f.maybe_active.remove(device) {
                        f.idle_cleared = false;
                    }
                }
            }
            Input::ConnectionIdle { device } => {
                for f in self.flying.values_mut() {
                    if f.maybe_active.remove(device) {
                        f.idle_cleared = true;
                    }
                }
            }
            _ => {}
        }
        let trace_input = self.trace.then(|| format!("{input:?}"));
        let cmds = self.core.step(self.now, input);
        if let Some(t) = trace_input {
            eprintln!("t={} {t}\n    -> {cmds:?}", self.now);
        }
        let flying_before = self.flying.len();
        let requested = cmds.contains(&Command::RequestLane);
        let mut starts: Vec<String> = Vec::new();
        for c in cmds {
            self.apply(c, &mut starts);
        }
        self.check_step(flying_before, requested, &starts);
        let followups = std::mem::take(&mut self.followups);
        for f in followups {
            match f {
                FollowUp::GrantLane => self.feed(Input::Lane { admitted: true }),
                FollowUp::LocalLanded(u, id, cv) => {
                    if self.flying.get(&u).is_some_and(|f| f.id == id) {
                        self.finish(u, id, cv, FetchResult::Landed);
                    }
                }
                FollowUp::YieldCut(u, id, cv) => {
                    if self.flying.get(&u).is_some_and(|f| f.id == id) {
                        self.finish(u, id, cv, FetchResult::Cancelled);
                    }
                }
            }
        }
    }

    /// Execute one command as the executor would, asserting the core-relative
    /// preconditions (the core can only be judged on what it was fed).
    fn apply(&mut self, c: Command, starts: &mut Vec<String>) {
        match c {
            Command::Start {
                key,
                fetch_id,
                content_version,
                blake3,
                providers,
                ..
            } => {
                let u = key.1.clone();
                assert!(
                    self.fed_storage,
                    "a Start while storage is unavailable ({u})"
                );
                assert!(
                    self.lane_admitted,
                    "a Start while the lane is not admitted ({u})"
                );
                assert!(
                    !self.flying.contains_key(&u),
                    "two fetches in flight for {u}"
                );
                assert!(
                    self.flying.len() < self.slots,
                    "a Start past the slot cap ({u}: {} in flight, {} slots)",
                    self.flying.len(),
                    self.slots
                );
                // I1 (core-relative): pins the version and hash it was fed
                let w = self
                    .fed_need
                    .get(&u)
                    .unwrap_or_else(|| panic!("I8: Start of {u}, which is not in the need set"));
                assert_eq!(
                    content_version, w.content_version,
                    "I1: Start pins the wanted version"
                );
                assert_eq!(blake3, w.blake3, "I1: Start pins the wanted hash");
                let w_since = w.since_ms;
                // I4/I5: ≥ 1 provider, each fed for THIS version, each
                // claiming THIS version at the hub
                assert!(!providers.is_empty(), "a providerless Start of {u}");
                assert!(
                    !self.core.frame_backing_off(&key, self.now),
                    "{u} started while its own back-off runs"
                );
                let (fed_at, fed) = self
                    .fed_providers
                    .get(&u)
                    .and_then(|by_version| by_version.get(&content_version))
                    .unwrap_or_else(|| {
                        panic!("I4/I5: Start of {u} v{content_version} with no list fed for it")
                    });
                if self.need_seq.get(&u).is_some_and(|(_, at)| fed_at < at) {
                    self.cov.starts_providers_first += 1;
                }
                let claimants = self.holders.claimants(seq_of(&u), content_version);
                for p in &providers {
                    assert!(fed.contains(p), "I5: {} was never fed for {u}", p.device);
                    assert!(
                        claimants.contains(&p.device.as_str()),
                        "I4: {} does not claim {u} v{content_version}",
                        p.device
                    );
                    assert!(
                        !self.core.device_backing_off(&p.device, self.now),
                        "§7.3: {} is backing off",
                        p.device
                    );
                }
                let devices: BTreeSet<String> = providers.into_iter().map(|p| p.device).collect();
                assert!(
                    self.flying.values().all(|f| f.id != fetch_id),
                    "fetch id {fetch_id} reused"
                );
                self.flying.insert(
                    u.clone(),
                    Fetch {
                        id: fetch_id,
                        cv: content_version,
                        providers: devices.clone(),
                        maybe_active: devices,
                        idle_cleared: false,
                    },
                );
                starts.push(u.clone());
                self.cov.starts += 1;
                if self.now.saturating_sub(w_since) >= STARVATION.as_millis() as i64 {
                    self.cov.starving_starts += 1;
                }
                // the executor's local shortcut (T15: adopt/link) — sometimes
                if self.rng.below(12) == 0 {
                    self.followups
                        .push(FollowUp::LocalLanded(u, fetch_id, content_version));
                }
            }
            Command::UpdateProviders {
                key,
                fetch_id,
                providers,
            } => {
                let u = key.1;
                let f = self
                    .flying
                    .get_mut(&u)
                    .unwrap_or_else(|| panic!("UpdateProviders for {u}, which is not in flight"));
                assert_eq!(f.id, fetch_id, "UpdateProviders names another fetch of {u}");
                match self.fed_providers.get(&u).and_then(|bv| bv.get(&f.cv)) {
                    Some((_, fed)) => {
                        for p in &providers {
                            assert!(fed.contains(p), "I5: {} was never fed for {u}", p.device);
                        }
                    }
                    None => assert!(
                        providers.is_empty(),
                        "I4: {u} v{} got providers of another version",
                        f.cv
                    ),
                }
                f.providers = providers.iter().map(|p| p.device.clone()).collect();
                self.cov.provider_updates += 1;
                f.maybe_active
                    .extend(providers.into_iter().map(|p| p.device));
            }
            Command::Cancel {
                key,
                fetch_id,
                reason,
            } => {
                let u = key.1;
                let f = self
                    .flying
                    .remove(&u)
                    .unwrap_or_else(|| panic!("Cancel of {u}, which is not in flight"));
                assert_eq!(f.id, fetch_id, "Cancel names another fetch of {u}");
                if reason == CancelReason::NoProvider && f.idle_cleared {
                    self.cov.idle_freed_slot += 1;
                }
                // the cancelled fetch may still report, late (I1: harmless)
                if self.rng.below(3) == 0 {
                    let r = match self.rng.below(4) {
                        0 => FetchResult::Landed,
                        1 => FetchResult::Failed,
                        2 => FetchResult::AwaitingGc,
                        _ => FetchResult::Cancelled,
                    };
                    self.late.push((u.clone(), f.id, f.cv, r));
                }
                match reason {
                    CancelReason::NewVersion => self.cov.cancel_new_version += 1,
                    CancelReason::NotWanted => self.cov.cancel_not_wanted += 1,
                    CancelReason::ProjectGone => self.cov.cancel_project_gone += 1,
                    CancelReason::StorageUnavailable => self.cov.cancel_storage += 1,
                    CancelReason::NoProvider => self.cov.cancel_no_provider += 1,
                }
                match reason {
                    CancelReason::NotWanted => assert!(
                        !self.fed_need.contains_key(&u),
                        "NotWanted cancel of {u}, still in the need set"
                    ),
                    CancelReason::NewVersion => assert!(
                        self.fed_need
                            .get(&u)
                            .is_some_and(|w| w.content_version != f.cv),
                        "NewVersion cancel of {u} v{} without a new version",
                        f.cv
                    ),
                    CancelReason::StorageUnavailable => {
                        assert!(
                            !self.fed_storage,
                            "StorageUnavailable cancel with storage up"
                        )
                    }
                    CancelReason::ProjectGone => {
                        assert!(!self.project_live, "ProjectGone cancel of a live project")
                    }
                    CancelReason::NoProvider => assert!(
                        f.providers.is_empty() && f.maybe_active.is_empty(),
                        "NoProvider cancel of {u} which may still be transferring: {f:?}"
                    ),
                }
            }
            Command::RequestLane => {
                assert!(!self.lane_admitted, "RequestLane while holding the lane");
                assert!(
                    !self.lane_yielded,
                    "RequestLane before the yielded permit went back"
                );
                assert!(
                    !self.lane_pending,
                    "a second RequestLane while one is pending"
                );
                assert!(self.flying.is_empty(), "RequestLane with fetches in flight");
                if self.personal {
                    self.lane_pending = true;
                } else {
                    self.followups.push(FollowUp::GrantLane);
                }
            }
            Command::ReleaseLane => {
                assert!(
                    self.lane_admitted || self.lane_yielded,
                    "ReleaseLane without the lane"
                );
                assert!(self.flying.is_empty(), "ReleaseLane with fetches in flight");
                self.lane_admitted = false;
                self.lane_yielded = false;
                self.cov.lane_releases += 1;
            }
        }
    }

    /// The core-relative invariants after one core step (R5).
    fn check_step(&self, flying_before: usize, requested: bool, starts: &[String]) {
        let core: Vec<(String, i32)> = self
            .core
            .in_flight()
            .into_iter()
            .map(|(k, cv)| (k.1, cv))
            .collect();
        let exec: Vec<(String, i32)> = self.flying.iter().map(|(u, f)| (u.clone(), f.cv)).collect();
        assert_eq!(
            core, exec,
            "core and executor disagree on the fetches in flight"
        );
        assert_eq!(
            self.core.lane(),
            self.lane_admitted,
            "core and executor disagree on the lane"
        );
        assert_eq!(
            self.core.lane_yielded(),
            self.lane_yielded,
            "core and executor disagree on the yielded permit"
        );
        if self.lane_yielded {
            assert!(
                !self.flying.is_empty(),
                "a drained yielded lane was not released"
            );
        }
        if !starts.is_empty() {
            assert!(self.flying.len() <= self.slots, "slot cap exceeded");
        }
        assert!(
            self.flying.len() <= flying_before.max(self.slots),
            "in flight grew past the slots"
        );
        if !self.fed_storage {
            assert!(
                self.flying.is_empty(),
                "fetching continues on an unavailable store"
            );
        }
        // every fetch is of a wanted frame at its wanted version: a frame
        // that left the need set or moved on was cancelled in THIS step
        for (u, f) in &self.flying {
            let w = self
                .fed_need
                .get(u)
                .unwrap_or_else(|| panic!("{u} left the need set but its fetch was not cancelled"));
            assert_eq!(
                w.content_version, f.cv,
                "{u}: a new version did not cancel v{}",
                f.cv
            );
        }
        if !self.fed_storage {
            return;
        }
        // work conservation, starvation and rarest-first (§7.2)
        let starving = STARVATION.as_millis() as i64;
        let mut idle: Vec<(String, usize, i64)> = Vec::new(); // (u, available, since)
        for (u, w) in &self.fed_need {
            if self.flying.contains_key(u) || self.core.frame_backing_off(&key(u), self.now) {
                continue;
            }
            let n = match self
                .fed_providers
                .get(u)
                .and_then(|bv| bv.get(&w.content_version))
            {
                Some((_, ps)) => ps
                    .iter()
                    .filter(|p| !self.core.device_backing_off(&p.device, self.now))
                    .count(),
                None => 0,
            };
            if n > 0 {
                idle.push((u.clone(), n, w.since_ms));
            }
        }
        if idle.is_empty() {
            return;
        }
        if self.lane_admitted {
            // every slot is taken by a fetch that may still be transferring:
            // pre-empting the dead providerless ones would free none (the
            // slots may have been lowered below what is in flight)
            let dead: Vec<&String> = self
                .flying
                .iter()
                .filter(|(_, f)| f.providers.is_empty() && f.maybe_active.is_empty())
                .map(|(u, _)| u)
                .collect();
            assert!(
                self.flying.len() - dead.len() >= self.slots,
                "work conservation: {idle:?} could start; {} in flight ({dead:?} dead), {} slots",
                self.flying.len(),
                self.slots
            );
        } else {
            assert!(
                requested || self.lane_pending || !self.flying.is_empty(),
                "work waits and the lane was not requested"
            );
        }
        let old = |since: i64| self.now.saturating_sub(since) >= starving;
        for s in starts {
            let w = &self.fed_need[s];
            let fed_s = &self.fed_providers[s][&w.content_version].1;
            let n_s = fed_s.len();
            for (u, n, since) in &idle {
                if old(*since) {
                    assert!(
                        old(w.since_ms) && w.since_ms <= *since,
                        "starvation: {s} started ahead of {u}, waiting since {since}"
                    );
                } else if !old(w.since_ms) {
                    // rarest first: the started frame had no more providers
                    // than a younger idle one (counts are at start time; a
                    // back-off cannot differ within the step)
                    let started_avail = fed_s
                        .iter()
                        .filter(|p| !self.core.device_backing_off(&p.device, self.now))
                        .count();
                    assert!(
                        started_avail <= *n,
                        "rarest first: {s} ({started_avail} of {n_s}) started ahead of {u} ({n})"
                    );
                }
            }
        }
    }

    /// The world-relative invariants once the executor has fed everything an
    /// event causes (I1, I5, I6, I8, I11).
    fn check_event(&self) {
        for (u, f) in &self.flying {
            assert_eq!(
                f.cv, self.frames[u].cv,
                "I6: {u} fetching superseded v{}",
                f.cv
            );
            assert!(
                self.in_replication_set(u),
                "I8: {u} fetched outside the replication set"
            );
            assert_eq!(
                self.local[u],
                Local::Wanted,
                "{u} fetched while {:?}",
                self.local[u]
            );
            let now: BTreeSet<String> =
                self.providers_of(u).into_iter().map(|p| p.device).collect();
            for d in &f.providers {
                assert!(
                    now.contains(d),
                    "I5/I11: {d} is no candidate for {u} v{} yet still in its live set",
                    f.cv
                );
            }
        }
        if !self.storage_ok {
            assert!(
                self.flying.is_empty(),
                "fetching continues on an unavailable store"
            );
        }
        // LIVENESS: storage up, a free slot and a lane that is held or can
        // be had ⇒ every wanted frame with a usable production-derived
        // candidate is in flight (or its lane request is pending)
        let lane_ok = self.lane_admitted || (!self.personal && !self.lane_yielded);
        if self.storage_ok
            && self.project_live
            && self.flying.len() < self.slots
            && lane_ok
            && !self.lane_pending
        {
            for w in self.need_set() {
                let u = &w.key.1;
                if self.flying.contains_key(u) || self.core.frame_backing_off(&w.key, self.now) {
                    continue;
                }
                let usable: Vec<String> = self
                    .providers_of(u)
                    .into_iter()
                    .map(|p| p.device)
                    .filter(|d| !self.core.device_backing_off(d, self.now))
                    .collect();
                assert!(
                    usable.is_empty(),
                    "liveness: {u} v{} is wanted, {usable:?} could serve it, a slot is free \
                     ({} of {} in flight) and the lane is {}, yet it is not in flight",
                    w.content_version,
                    self.flying.len(),
                    self.slots,
                    if self.lane_admitted {
                        "held"
                    } else {
                        "free to request"
                    }
                );
            }
        }
    }

    // --------------------------------------------------------------- events

    /// Fetch `id` of `u` (version `cv`) ends — the fetch in flight, or a
    /// cancelled one reporting late. The landing fence records only the
    /// current version of a wanted frame (I1), whichever fetch landed it.
    fn finish(&mut self, u: String, id: u64, cv: i32, result: FetchResult) {
        let mut result = result;
        if result == FetchResult::Landed {
            if cv == self.frames[&u].cv && self.local[&u] == Local::Wanted {
                self.local.insert(u.clone(), Local::Held(cv));
                self.holders.apply_delta(&HolderDeltaWire {
                    device: ME.into(),
                    add: vec![(seq_of(&u), cv)],
                    rm: vec![],
                });
            } else {
                // the fence refused (T15 maps `Stale` to Cancelled)
                result = FetchResult::Cancelled;
            }
        }
        match result {
            FetchResult::Landed => self.cov.landed += 1,
            FetchResult::Failed => self.cov.failed += 1,
            _ => {}
        }
        if result == FetchResult::AwaitingGc && self.local[&u] == Local::Wanted {
            self.local.insert(u.clone(), Local::Parked);
        }
        self.feed(Input::Finished {
            key: key(&u),
            fetch_id: id,
            result,
        });
        self.feed_need();
    }

    /// A frame (re-)entered the need set: the executor feeds its need set
    /// and its provider list, in a seeded order (R1).
    fn feed_entry(&mut self, u: &str) {
        let providers_first = self.rng.below(2) == 0;
        if providers_first && self.project_live {
            self.feed_providers(u);
        }
        self.feed_need();
        if !providers_first && self.project_live {
            self.feed_providers(u);
        }
    }

    fn crash(&mut self) {
        self.core = Core::new(self.rng.next_u64(), self.slots);
        self.flying.clear();
        self.lane_admitted = false;
        self.lane_yielded = false;
        self.lane_pending = false;
        self.personal = false;
        self.late.clear();
        self.fed_need.clear();
        self.fed_providers.clear();
        self.need_seq.clear();
        self.fed_storage = true;
        self.feed(Input::Storage {
            fetching: self.storage_ok,
        });
        self.resync_all();
    }

    /// Advance the clock; the executor's timer feeds `Tick` at every wake.
    fn advance(&mut self) {
        let dt = if self.rng.below(25) == 0 {
            self.rng.below(900_000) as i64
        } else {
            1 + self.rng.below(5_000) as i64
        };
        let target = self.now + dt;
        while let Some(wake) = self.core.next_wake_ms() {
            if wake > target {
                break;
            }
            self.now = self.now.max(wake);
            self.feed(Input::Tick);
        }
        self.now = target;
    }

    /// One random event. Returns a label for the failure message.
    fn step(&mut self) -> &'static str {
        self.advance();
        // outages are short: storage remounts, the project is re-joined and
        // a personal transfer ends, each with 1/4 per step
        if !self.storage_ok && self.rng.below(4) == 0 {
            self.storage_ok = true;
            self.feed(Input::Storage { fetching: true });
        }
        if !self.project_live && self.rng.below(4) == 0 {
            self.project_live = true;
            self.resync_all();
        }
        if self.personal && self.rng.below(4) == 0 {
            self.personal = false;
            if self.lane_pending {
                self.lane_pending = false;
                self.feed(Input::Lane { admitted: true });
            }
        }
        if !self.late.is_empty() && self.rng.below(2) == 0 {
            let i = self.rng.below(self.late.len());
            let (lu, id, cv, r) = self.late.remove(i);
            self.finish(lu, id, cv, r);
        }
        let us: Vec<String> = self.frames.keys().cloned().collect();
        let u = us[self.rng.below(us.len())].clone();
        let dev = PEERS[self.rng.below(PEERS.len())].to_string();
        match self.rng.below(32) {
            0..=2 => {
                // a holder claims a version (current, or an older one)
                let cur = self.frames[&u].cv;
                let cv = if self.rng.below(3) == 0 {
                    (cur - 1).max(1)
                } else {
                    cur
                };
                self.holders.apply_delta(&HolderDeltaWire {
                    device: dev,
                    add: vec![(seq_of(&u), cv)],
                    rm: vec![],
                });
                if self.project_live {
                    self.feed_providers(&u);
                }
                "holders add"
            }
            3 => {
                self.holders.apply_delta(&HolderDeltaWire {
                    device: dev,
                    add: vec![],
                    rm: vec![seq_of(&u)],
                });
                if self.project_live {
                    self.feed_providers(&u);
                }
                "holders rm"
            }
            4..=6 => {
                let connected = self.rng.below(4) != 0;
                let serving = connected && self.rng.below(5) != 0;
                self.presence.apply_event(&PresenceEvent {
                    project_id: PID.into(),
                    replace: false,
                    changes: vec![PresenceChange {
                        device: dev,
                        connected,
                        serving,
                        relay_url: None,
                    }],
                });
                self.resync_all();
                "presence"
            }
            7 | 26 | 27 => {
                // I10: a CAS version bump; the hub writes the publisher's
                // first holding in the same transaction (I4)
                let cv = {
                    let f = self.frames.get_mut(&u).expect("known frame");
                    f.cv += 1;
                    f.cv
                };
                self.holders.apply_delta(&HolderDeltaWire {
                    device: dev,
                    add: vec![(seq_of(&u), cv)],
                    rm: vec![],
                });
                if let Local::Held(_) = self.local[&u] {
                    self.set_wanted(&u); // Held(vN) → Wanted(vN+1); vN stays (L7)
                } else if self.local[&u] == Local::Wanted {
                    self.wanted_since.insert(u.clone(), self.now);
                }
                self.resync_all();
                "new version"
            }
            8 => {
                let accepted = {
                    let f = self.frames.get_mut(&u).expect("known frame");
                    f.accepted = !f.accepted;
                    f.accepted
                };
                if !accepted {
                    if matches!(self.local[&u], Local::Wanted | Local::Held(_)) {
                        self.local.insert(u.clone(), Local::Idle);
                    }
                } else if self.local[&u] == Local::Idle {
                    self.set_wanted(&u);
                }
                self.resync_all();
                "exclusion toggled"
            }
            9..=12 => {
                if !self.flying.is_empty() {
                    let fs: Vec<String> = self.flying.keys().cloned().collect();
                    let fu = fs[self.rng.below(fs.len())].clone();
                    let f = self.flying[&fu].clone();
                    let result = if f.providers.is_empty() && f.maybe_active.is_empty() {
                        // nobody to fetch from: the run gives up or waits
                        if self.rng.below(2) == 0 {
                            FetchResult::Failed
                        } else {
                            return "fetch waits";
                        }
                    } else {
                        match self.rng.below(20) {
                            0..=11 => FetchResult::Landed,
                            12..=15 => FetchResult::Failed, // incl. refused by every provider
                            16..=18 => FetchResult::Cancelled,
                            _ => FetchResult::AwaitingGc,
                        }
                    };
                    self.finish(fu, f.id, f.cv, result);
                }
                "fetch finished"
            }
            13 => {
                if self.storage_ok {
                    self.storage_ok = false;
                    self.feed(Input::Storage { fetching: false });
                }
                "storage unmounted"
            }
            14 => {
                // a replica edited on disk → quarantined (L5, P13), or
                // deleted → re-fetched after the settle window; a user's
                // "refetch the original" makes a quarantined one wanted again
                match self.local[&u] {
                    Local::Held(_) if self.rng.below(2) == 0 => {
                        self.local.insert(u.clone(), Local::Quarantined);
                    }
                    Local::Held(_) => self.set_wanted(&u),
                    Local::Quarantined => self.set_wanted(&u),
                    _ => return "edit (nothing held)",
                }
                if self.local[&u] == Local::Wanted {
                    self.feed_entry(&u);
                } else {
                    self.feed_need();
                }
                "edited / refetched"
            }
            15 => {
                // I11: a member leaves (or comes back)
                if !self.members.remove(&dev) {
                    self.members.insert(dev);
                }
                self.resync_all();
                "membership changed"
            }
            16 => {
                self.feed(Input::DialFailed { device: dev });
                "dial failed"
            }
            17 => {
                self.feed(Input::ConnectionClosed { device: dev });
                "connection closed"
            }
            18 => {
                self.feed(Input::DialOk { device: dev });
                "dial ok"
            }
            19 => {
                // C24: local state survives a crash, Fetching does not
                self.crash();
                "crash"
            }
            20 => {
                // a personal transfer arrives: the gate yields the collab
                // lane (it ends at the top of a later step)
                if !self.personal {
                    self.personal = true;
                    if self.lane_admitted {
                        // the live run cuts every item not transferring at once
                        for (fu, f) in &self.flying {
                            if f.providers.is_empty() && f.maybe_active.is_empty() {
                                self.followups
                                    .push(FollowUp::YieldCut(fu.clone(), f.id, f.cv));
                            }
                        }
                        self.cov.yields += 1;
                        self.feed(Input::Lane { admitted: false });
                    }
                }
                "personal transfer waits"
            }
            21 => {
                self.slots = 1 + self.rng.below(4);
                if self.slots < self.flying.len() {
                    self.cov.slots_below_in_flight += 1;
                }
                self.feed(Input::Slots(self.slots));
                "slots changed"
            }
            22 => {
                self.feed(Input::ClearBackoffs);
                "sync now"
            }
            23 => {
                // a late provider list of an older version (R1): kept under
                // its own version, never used for the current want
                let cv = self.frames[&u].cv - 1;
                if cv >= 1 && self.project_live {
                    let providers = self.providers_at(&u, cv);
                    self.feed(Input::Providers {
                        key: key(&u),
                        content_version: cv,
                        providers,
                    });
                }
                "stale provider list"
            }
            24 => {
                if self.project_live {
                    self.project_live = false;
                    self.feed(Input::ProjectGone {
                        project_id: PID.into(),
                    });
                }
                "project gone"
            }
            25 => {
                // the GC ran over a parked frame, or a user flips "keep"
                match self.local[&u] {
                    Local::Parked | Local::NotKept => {
                        self.set_wanted(&u);
                        self.feed_entry(&u);
                    }
                    Local::Wanted => {
                        self.local.insert(u.clone(), Local::NotKept);
                        self.feed_need();
                    }
                    _ => {
                        self.feed(Input::Tick);
                        return "tick";
                    }
                }
                "keep / gc"
            }
            28..=30 => {
                // our own pool closes an idle connection: no strike
                self.feed(Input::ConnectionIdle { device: dev });
                "idle close"
            }
            _ => {
                // an epoch restore (spec §4.4) lowers a frame's version; the
                // restored hub holds that version's claims again
                let cur = self.frames[&u].cv;
                if cur <= 1 {
                    self.feed(Input::Tick);
                    return "tick";
                }
                if self
                    .fed_need
                    .get(&u)
                    .is_some_and(|w| w.content_version == cur)
                {
                    self.cov.version_down += 1;
                }
                self.frames.get_mut(&u).expect("known frame").cv = cur - 1;
                self.holders.apply_delta(&HolderDeltaWire {
                    device: dev,
                    add: vec![(seq_of(&u), cur - 1)],
                    rm: vec![],
                });
                match self.local[&u] {
                    Local::Held(v) if v != cur - 1 => self.set_wanted(&u),
                    Local::Wanted => {
                        self.wanted_since.insert(u.clone(), self.now);
                    }
                    _ => {}
                }
                self.resync_all();
                "version restored down"
            }
        }
    }
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "<non-string panic>".into())
}

/// Runs one seed; returns what it exercised.
fn run_seed(seed: u64) -> Coverage {
    let mut w = World::new(seed);
    let setup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        w.feed(Input::Slots(w.slots));
        w.resync_all();
        w.check_event();
    }));
    if let Err(e) = setup {
        panic!(
            "collab scheduler simulation failed: seed {seed}, setup: {}",
            panic_text(&*e)
        );
    }
    let mut last = "setup";
    for i in 0..STEPS {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let label = w.step();
            if w.trace {
                eprintln!("== step {i}: {label}");
            }
            w.check_event();
            label
        }));
        match r {
            Ok(label) => last = label,
            Err(e) => panic!(
                "collab scheduler simulation failed: seed {seed}, step {i} (previous event \"{last}\"): {}",
                panic_text(&*e)
            ),
        }
        w.cov.busy_steps += usize::from(!w.flying.is_empty());
    }
    w.cov
}

#[test]
fn seeded_interleavings_keep_every_invariant() {
    if let Ok(s) = std::env::var("COLLAB_SIM_SEED") {
        run_seed(s.parse().expect("COLLAB_SIM_SEED is a number"));
        return;
    }
    let t0 = std::time::Instant::now();
    let mut cov = Coverage::default();
    for seed in 0..SEEDS {
        cov.add(&run_seed(seed));
    }
    eprintln!(
        "collab scheduler simulation: {SEEDS} seeds x {STEPS} steps in {:?}: {cov:?}",
        t0.elapsed()
    );
    // the simulation must exercise every path, not idle past them
    let total = SEEDS as usize * STEPS;
    assert!(
        cov.busy_steps * 5 > total,
        "fetching in too few steps: {cov:?}"
    );
    for (what, n) in [
        ("starts", cov.starts),
        ("landings", cov.landed),
        ("failures", cov.failed),
        ("starving starts", cov.starving_starts),
        ("provider updates", cov.provider_updates),
        ("NewVersion cancels", cov.cancel_new_version),
        ("NotWanted cancels", cov.cancel_not_wanted),
        ("ProjectGone cancels", cov.cancel_project_gone),
        ("StorageUnavailable cancels", cov.cancel_storage),
        ("NoProvider cancels", cov.cancel_no_provider),
        ("yields", cov.yields),
        ("lane releases", cov.lane_releases),
        (
            "starts on lists fed before the need set",
            cov.starts_providers_first,
        ),
        ("slots lowered below in-flight", cov.slots_below_in_flight),
        ("late results ignored", cov.late_ignored),
        ("idle closes freeing a slot", cov.idle_freed_slot),
        ("versions going down", cov.version_down),
    ] {
        assert!(n >= 10, "the simulation barely exercised {what}: {cov:?}");
    }
}

/// Seed 120 (found by this simulation): slots lowered to 2 with three
/// fetches in flight, then one fetch's only connection closed — the core
/// pre-empted that dead fetch and started a new one past the cap. The core
/// now frees a whole slot or starts (and cancels) nothing.
#[test]
fn regression_seed_120_a_pre_emption_never_starts_past_lowered_slots() {
    run_seed(120);
}

/// I2/I3: the feed cursor over a hub that keeps writing while its stream
/// reorders, duplicates and drops events, reconnects (hello), sends the 60 s
/// versions vector and once rotates its epoch (a restore to an earlier
/// state). The client acts only through the production `cursor::step`,
/// `plan_hello` and `plan_versions`, catching up over "REST" (the hub's
/// state at its head at that moment). After every action the client's state
/// is exactly the hub's state as of the client's cursor on the client's
/// epoch; no event applies twice; the cursor never moves back inside an
/// epoch; and a final hello + versions round converges to the hub's head.
#[test]
fn feed_cursors_converge_through_catch_up_gaps_duplicates_reordering_and_an_epoch_change() {
    use crate::collab::live::cursor::{
        plan_hello, plan_versions, step as cursor_step, FeedCursor, HolderPlan, Step, VersionsPlan,
    };

    /// One axis (`project` versions or `holders` seqs) of one epoch: write
    /// `v` (1-based) sets `key = v`.
    #[derive(Clone, Default)]
    struct Log(Vec<u8>);
    impl Log {
        fn head(&self) -> i64 {
            self.0.len() as i64
        }
        fn state_at(&self, h: i64) -> BTreeMap<u8, i64> {
            let mut m = BTreeMap::new();
            for (i, k) in self.0.iter().take(h.max(0) as usize).enumerate() {
                m.insert(*k, i as i64 + 1);
            }
            m
        }
    }
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
    enum Axis {
        Project,
        Holders,
    }
    struct Hub {
        epoch: u32,
        /// every epoch's logs: old epochs are frozen at their rotation
        logs: BTreeMap<u32, (Log, Log)>,
    }
    impl Hub {
        fn log(&self, e: u32, a: Axis) -> &Log {
            let l = &self.logs[&e];
            match a {
                Axis::Project => &l.0,
                Axis::Holders => &l.1,
            }
        }
        fn cur(&self, a: Axis) -> &Log {
            self.log(self.epoch, a)
        }
    }
    struct Client {
        cursor: FeedCursor,
        /// the epoch this client's state belongs to (None before the first load)
        on: Option<u32>,
        project: BTreeMap<u8, i64>,
        holders: BTreeMap<u8, i64>,
        applied: BTreeSet<(Axis, i64)>,
    }
    impl Client {
        fn cursor_of(&self, a: Axis) -> i64 {
            match a {
                Axis::Project => self.cursor.version,
                Axis::Holders => self.cursor.holder_seq,
            }
        }
        fn state(&mut self, a: Axis) -> &mut BTreeMap<u8, i64> {
            match a {
                Axis::Project => &mut self.project,
                Axis::Holders => &mut self.holders,
            }
        }
        fn set_cursor(&mut self, a: Axis, v: i64) {
            match a {
                Axis::Project => self.cursor.version = v,
                Axis::Holders => self.cursor.holder_seq = v,
            }
        }
        /// REST catch-up of one axis: the hub's state at its head NOW.
        fn catch_up(&mut self, hub: &Hub, a: Axis) {
            let log = hub.cur(a);
            *self.state(a) = log.state_at(log.head());
            self.set_cursor(a, log.head());
        }
        /// A snapshot reload of everything (epoch change).
        fn reload(&mut self, hub: &Hub) {
            self.catch_up(hub, Axis::Project);
            self.catch_up(hub, Axis::Holders);
            self.cursor.epoch = Some(format!("e{}", hub.epoch));
            self.on = Some(hub.epoch);
            self.applied.clear();
        }
        fn hello(&mut self, hub: &Hub) {
            let hp = hub.cur(Axis::Project).head();
            let hh = hub.cur(Axis::Holders).head();
            let plan = plan_hello(&self.cursor, &format!("e{}", hub.epoch), hp, hh);
            if plan.epoch_changed {
                self.reload(hub);
                return;
            }
            if plan.catch_up_project {
                self.catch_up(hub, Axis::Project);
            }
            match plan.holders {
                HolderPlan::InSync => {}
                HolderPlan::Delta | HolderPlan::Snapshot => self.catch_up(hub, Axis::Holders),
            }
            // the first hello confirms the epoch the state now belongs to
            if self.on.is_none() {
                self.cursor.epoch = Some(format!("e{}", hub.epoch));
                self.on = Some(hub.epoch);
            }
        }
        fn versions(&mut self, hub: &Hub) {
            let hp = hub.cur(Axis::Project).head();
            let hh = hub.cur(Axis::Holders).head();
            match plan_versions(&self.cursor, hp, hh) {
                VersionsPlan::InSync => {}
                VersionsPlan::CatchUp { project, holders } => {
                    if project {
                        self.catch_up(hub, Axis::Project);
                    }
                    if holders {
                        self.catch_up(hub, Axis::Holders);
                    }
                }
                VersionsPlan::EpochChange => self.reload(hub),
            }
        }
        /// One delivered stream event `(axis, prev, head, key)`.
        fn deliver(&mut self, hub: &Hub, a: Axis, prev: i64, v: i64, k: u8, seed: u64) {
            let before = self.cursor_of(a);
            match cursor_step(before, prev, v) {
                Step::Apply => {
                    assert!(
                        self.applied.insert((a, v)),
                        "seed {seed}: {a:?} event {v} applied twice"
                    );
                    self.state(a).insert(k, v);
                    self.set_cursor(a, v);
                }
                Step::Ignore => {}
                Step::CatchUp => self.catch_up(hub, a),
            }
            assert!(
                self.cursor_of(a) >= before,
                "seed {seed}: {a:?} cursor went back"
            );
        }
    }

    for seed in 0..300u64 {
        let mut rng = SplitMix64(seed.wrapping_mul(0x9E37_79B9) ^ 0xC0FFEE);
        let mut hub = Hub {
            epoch: 1,
            logs: BTreeMap::from([(1, (Log::default(), Log::default()))]),
        };
        let mut client = Client {
            cursor: FeedCursor {
                epoch: None,
                version: 0,
                holder_seq: -1,
            },
            on: None,
            project: BTreeMap::new(),
            holders: BTreeMap::new(),
            applied: BTreeSet::new(),
        };
        // the stream: (axis, prev, head, key), in flight to the client
        let mut wire: Vec<(Axis, i64, i64, u8)> = Vec::new();
        let mut session = false;
        let mut rotated = 0;
        for _ in 0..400 {
            match rng.below(20) {
                0..=6 => {
                    // the hub commits a write; the stream carries it (maybe
                    // lost, maybe twice)
                    let a = if rng.below(3) == 0 {
                        Axis::Holders
                    } else {
                        Axis::Project
                    };
                    let k = rng.below(6) as u8;
                    let e = hub.epoch;
                    let log = hub.logs.get_mut(&e).expect("current epoch");
                    let l = match a {
                        Axis::Project => &mut log.0,
                        Axis::Holders => &mut log.1,
                    };
                    let prev = l.head();
                    l.0.push(k);
                    if session && rng.below(6) != 0 {
                        wire.push((a, prev, prev + 1, k));
                        if rng.below(5) == 0 {
                            wire.push((a, prev, prev + 1, k));
                        }
                    }
                }
                7..=13 => {
                    // one delivery, out of order within a small window
                    if session && !wire.is_empty() {
                        let i = rng.below(wire.len().min(3));
                        let (a, prev, v, k) = wire.remove(i);
                        client.deliver(&hub, a, prev, v, k, seed);
                    }
                }
                14 | 15 => {
                    // reconnect: the old stream's backlog is gone, hello first
                    wire.clear();
                    session = true;
                    client.hello(&hub);
                }
                16 | 17 => {
                    if session {
                        client.versions(&hub);
                    }
                }
                18 => {
                    // an epoch rotation: a restore to an earlier state; every
                    // stream is closed and the hub goes on from there
                    if rotated < 2 && rng.below(3) == 0 {
                        rotated += 1;
                        let (p, h) = hub.logs[&hub.epoch].clone();
                        let cut = |l: &Log, rng: &mut SplitMix64| {
                            let keep = rng.below(l.0.len() + 1);
                            Log(l.0[..keep].to_vec())
                        };
                        let restored = (cut(&p, &mut rng), cut(&h, &mut rng));
                        hub.epoch += 1;
                        hub.logs.insert(hub.epoch, restored);
                        wire.clear();
                        session = false;
                    }
                }
                _ => {
                    // an idle second: nothing but time
                }
            }
            // consistency: the client's state is the hub's state at the
            // client's cursor on the client's own epoch
            if let Some(e) = client.on {
                let p = hub.log(e, Axis::Project).state_at(client.cursor.version);
                let h = hub.log(e, Axis::Holders).state_at(client.cursor.holder_seq);
                assert_eq!(
                    client.project, p,
                    "seed {seed}: project state off its cursor"
                );
                assert_eq!(
                    client.holders, h,
                    "seed {seed}: holder state off its cursor"
                );
            }
        }
        // the session repairs everything still missing
        client.hello(&hub);
        client.versions(&hub);
        assert_eq!(
            client.on,
            Some(hub.epoch),
            "seed {seed}: client on a stale epoch"
        );
        assert_eq!(client.cursor.epoch, Some(format!("e{}", hub.epoch)));
        assert_eq!(
            client.cursor.version,
            hub.cur(Axis::Project).head(),
            "seed {seed}"
        );
        assert_eq!(
            client.cursor.holder_seq,
            hub.cur(Axis::Holders).head(),
            "seed {seed}"
        );
        let p = hub.cur(Axis::Project);
        let h = hub.cur(Axis::Holders);
        assert_eq!(client.project, p.state_at(p.head()), "seed {seed}");
        assert_eq!(client.holders, h.state_at(h.head()), "seed {seed}");
    }
}

/// I9: the serve decision never serves a changed, missing or unavailable
/// file, and a refusal is never counted against the stream limit.
#[test]
fn the_serve_check_never_serves_a_changed_file() {
    use crate::collab::serve::{decide, ServeDecision, ServeRecord};
    use crate::collab::storage::sweep::{Stamp, MTIME_TOLERANCE_SECS};
    let mut rng = SplitMix64(7);
    for _ in 0..10_000 {
        let rec = ServeRecord {
            project_id: "p".into(),
            frame_uuid: "u".into(),
            path: "/x".into(),
            stamp: Stamp {
                size: 100,
                mtime: 1_000,
            },
        };
        let observed = match rng.below(5) {
            0 => None,
            1 => Some(Stamp {
                size: 100,
                mtime: 1_000 - 4 + rng.below(9) as i64,
            }),
            2 => Some(Stamp {
                size: 99 + rng.below(3) as u64,
                mtime: 1_000,
            }),
            _ => Some(Stamp {
                size: 100,
                mtime: 1_000 + rng.below(200) as i64 - 100,
            }),
        };
        let held = rng.below(6) != 0;
        let serving = rng.below(6) != 0;
        let limit = 1 + rng.below(4);
        let in_use = rng.below(6);
        let d = decide(held.then_some(&rec), observed, serving, in_use, limit);
        // an independent reading of "intact": same size, mtime within the
        // tolerance
        let intact = observed
            .is_some_and(|o| o.size == 100 && (o.mtime - 1_000).abs() <= MTIME_TOLERANCE_SECS);
        let expected = if !serving {
            ServeDecision::RefuseUnavailable
        } else if !held {
            ServeDecision::RefuseNotHeld
        } else if !intact {
            ServeDecision::RefuseMismatch
        } else if in_use >= limit {
            ServeDecision::RefuseLimit
        } else {
            ServeDecision::Serve
        };
        assert_eq!(
            d, expected,
            "{observed:?} held={held} serving={serving} {in_use}/{limit}"
        );
    }
}
