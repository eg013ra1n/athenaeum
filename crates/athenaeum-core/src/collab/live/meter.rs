//! Live exchange telemetry (spec 2026-09-29 §6): bytes, rate and frames in
//! flight per (project, peer device, direction). In memory only, written from
//! the fetch loop and the serve loop, read by the runtime's progress event and
//! the `get_collab_exchange` snapshot. Never a correctness input.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const RATE_WINDOW: Duration = Duration::from_secs(10);
pub const MOVING: Duration = Duration::from_secs(3);
pub const IDLE_DROP: Duration = Duration::from_secs(60);
pub const PROGRESS_PERIOD: Duration = Duration::from_secs(1);
const SAMPLE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum FlowDirection {
    Recv,
    Send,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct InFlightView {
    pub frame_uuid: String,
    pub file_name: String,
    pub size: i64,
    pub done: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FlowView {
    pub project_id: String,
    pub device: String,
    pub direction: FlowDirection,
    pub bytes_session: i64,
    pub rate_bps: f64,
    pub eta_secs: Option<f64>,
    pub moving: bool,
    pub completed: i64,
    pub in_flight: Vec<InFlightView>,
}

struct Item {
    dir: FlowDirection,
    project_id: String,
    frame_uuid: String,
    file_name: String,
    size: u64,
    per_device: HashMap<String, u64>,
}

struct Flow {
    bytes_session: u64,
    completed: u64,
    rate: f64,
    sample_at: Instant,
    sample_bytes: u64,
    last_moved: Instant,
}

type FlowKey = (String, String, FlowDirection);

#[derive(Default)]
struct State {
    items: HashMap<String, Item>,
    flows: BTreeMap<FlowKey, Flow>,
}

/// In-memory bytes/rate/in-flight telemetry for the live exchange. Never a
/// correctness input — a dropped or stale flow only degrades what the UI
/// shows, never what the exchange does.
#[derive(Default)]
pub struct ExchangeMeter {
    state: Mutex<State>,
    /// Woken when a flow starts moving (a new flow, or one re-seeded after a
    /// pause): the deltas land here from the fetch and serve loops, never
    /// through the live runtime's loop, which would otherwise sleep until
    /// its next timer and miss the start of a transfer.
    started: tokio::sync::Notify,
}

impl ExchangeMeter {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let mut g = match self.state.lock() {
            Ok(g) => g,
            Err(p) => {
                tracing::warn!("exchange meter lock poisoned; continuing with its data");
                p.into_inner()
            }
        };
        f(&mut g)
    }

    /// Registers a new in-flight item (a frame's file, one direction). A
    /// second `register` for the same key replaces the first.
    pub fn register(
        &self,
        key: &str,
        dir: FlowDirection,
        project_id: &str,
        frame_uuid: &str,
        file_name: &str,
        size: u64,
    ) {
        self.with(|s| {
            s.items.insert(
                key.to_string(),
                Item {
                    dir,
                    project_id: project_id.to_string(),
                    frame_uuid: frame_uuid.to_string(),
                    file_name: file_name.to_string(),
                    size,
                    per_device: HashMap::new(),
                },
            );
        });
    }

    /// Updates an item's known size once it becomes known mid-flight (a serve
    /// started before the file's size was on hand).
    pub fn set_size(&self, key: &str, size: u64) {
        self.with(|s| {
            if let Some(i) = s.items.get_mut(key) {
                i.size = size;
            }
        });
    }

    /// Records a delivered chunk for `key` from/to `device`. A key that was
    /// never `register`ed is ignored.
    pub fn delivered(&self, key: &str, device: &str, delta: u64, now: Instant) {
        if delta == 0 {
            return;
        }
        let started = self.with(|s| {
            let Some(item) = s.items.get_mut(key) else {
                return false;
            };
            *item.per_device.entry(device.to_string()).or_default() += delta;
            let fk = (item.project_id.clone(), device.to_string(), item.dir);
            let existed = s.flows.contains_key(&fk);
            let flow = s.flows.entry(fk).or_insert_with(|| Flow {
                bytes_session: 0,
                completed: 0,
                rate: 0.0,
                sample_at: now,
                sample_bytes: 0,
                last_moved: now,
            });
            // A flow that's been quiet (no delivery) for longer than
            // `MOVING` re-seeds exactly like a brand-new one: without this,
            // a paused flow's frozen `sample_at`/`rate` would let the
            // resumed delta's `dt` span the whole idle gap, producing a
            // wildly wrong rate/ETA for ~`RATE_WINDOW`-scaled seconds after
            // every pause (a stalled connection, a backoff retry, …).
            let was_quiet = existed && now.saturating_duration_since(flow.last_moved) > MOVING;
            flow.bytes_session += delta;
            flow.last_moved = now;
            // A brand-new (or just-re-seeded) flow's own delta anchors
            // `sample_at` at this instant (zero elapsed time behind it) —
            // folding it into `sample_bytes` too would double-count it
            // against the NEXT window's duration and transiently spike the
            // rate. Only a flow that already existed AND wasn't quiet
            // accumulates into the sample window this call.
            if !existed || was_quiet {
                flow.rate = 0.0;
                flow.sample_at = now;
                flow.sample_bytes = 0;
            } else {
                flow.sample_bytes += delta;
                let dt = now.saturating_duration_since(flow.sample_at);
                if dt >= SAMPLE {
                    let inst = flow.sample_bytes as f64 / dt.as_secs_f64();
                    let alpha = 1.0 - (-dt.as_secs_f64() / RATE_WINDOW.as_secs_f64()).exp();
                    flow.rate = if flow.rate == 0.0 {
                        inst
                    } else {
                        flow.rate + alpha * (inst - flow.rate)
                    };
                    flow.sample_at = now;
                    flow.sample_bytes = 0;
                }
            }
            !existed || was_quiet
        });
        if started {
            self.started.notify_one();
        }
    }

    /// Resolves once a flow started moving since the last wait (a start seen
    /// while nobody waits is kept for the next wait — one permit).
    pub async fn flow_started(&self) {
        self.started.notified().await;
    }

    /// True while a progress event may be owed: an item has delivered bytes
    /// in flight, or a flow moved within [`MOVING`].
    pub fn needs_progress(&self, now: Instant) -> bool {
        self.with(|s| {
            s.items.values().any(|i| !i.per_device.is_empty())
                || s.flows
                    .values()
                    .any(|f| now.saturating_duration_since(f.last_moved) <= MOVING)
        })
    }

    /// Ends an item; returns its per-device bytes, largest first. `completed`
    /// credits one completion to the top device's flow.
    pub fn finish(&self, key: &str, completed: bool, now: Instant) -> Vec<(String, u64)> {
        self.with(|s| {
            let Some(item) = s.items.remove(key) else {
                return Vec::new();
            };
            let mut by: Vec<(String, u64)> = item.per_device.into_iter().collect();
            by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            if completed {
                if let Some((top, _)) = by.first() {
                    if let Some(f) =
                        s.flows
                            .get_mut(&(item.project_id.clone(), top.clone(), item.dir))
                    {
                        f.completed += 1;
                        f.last_moved = now;
                    }
                }
            }
            by
        })
    }

    /// True while any registered item still has at least one device with
    /// delivered bytes.
    pub fn any_in_flight(&self) -> bool {
        self.with(|s| s.items.values().any(|i| !i.per_device.is_empty()))
    }

    /// Renders the current flows, pruning any flow that has been idle past
    /// [`IDLE_DROP`] and has no item currently in flight against it.
    pub fn snapshot(&self, now: Instant) -> Vec<FlowView> {
        self.with(|s| {
            let busy: BTreeSet<FlowKey> = s
                .items
                .values()
                .flat_map(|i| {
                    i.per_device
                        .keys()
                        .map(move |d| (i.project_id.clone(), d.clone(), i.dir))
                })
                .collect();
            s.flows.retain(|k, f| {
                busy.contains(k) || now.saturating_duration_since(f.last_moved) <= IDLE_DROP
            });
            s.flows
                .iter()
                .map(|((pid, dev, dir), f)| {
                    let moving = now.saturating_duration_since(f.last_moved) <= MOVING;
                    let rate = if moving { f.rate } else { 0.0 };
                    let mut in_flight: Vec<InFlightView> = s
                        .items
                        .values()
                        .filter(|i| &i.project_id == pid && i.dir == *dir)
                        .filter_map(|i| i.per_device.get(dev).map(|d| (i, *d)))
                        .map(|(i, d)| InFlightView {
                            frame_uuid: i.frame_uuid.clone(),
                            file_name: i.file_name.clone(),
                            size: i.size as i64,
                            // A hedged overlap (or a retry re-delivering
                            // past what a known size expects) must not read
                            // above 100%; with `size == 0` (serve before
                            // `Started`) there's nothing to clamp to, so it
                            // shows raw bytes.
                            done: (if i.size > 0 { d.min(i.size) } else { d }) as i64,
                        })
                        .collect();
                    in_flight.sort_by(|a, b| a.frame_uuid.cmp(&b.frame_uuid));
                    let remaining: i64 = in_flight.iter().map(|x| (x.size - x.done).max(0)).sum();
                    FlowView {
                        project_id: pid.clone(),
                        device: dev.clone(),
                        direction: *dir,
                        bytes_session: f.bytes_session as i64,
                        rate_bps: rate,
                        eta_secs: (rate > 0.0 && remaining > 0).then(|| remaining as f64 / rate),
                        moving,
                        completed: f.completed as i64,
                        in_flight,
                    }
                })
                .collect()
        })
    }
}

/// When the runtime emits `collab-exchange-progress`: at most once per
/// [`PROGRESS_PERIOD`] while something moves, then exactly once more for each
/// project that went quiet (the zero payload).
#[derive(Default)]
pub struct ProgressGate {
    last: Option<Instant>,
    active: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateEmit {
    pub projects: Vec<String>,
    pub quiet: Vec<String>,
}

impl ProgressGate {
    /// True while at least one project is considered actively moving (i.e. a
    /// quiet emission is still owed for it).
    pub fn armed(&self) -> bool {
        !self.active.is_empty()
    }

    pub fn poll(&mut self, now: Instant, moving: BTreeSet<String>) -> Option<GateEmit> {
        if self
            .last
            .is_some_and(|l| now.saturating_duration_since(l) < PROGRESS_PERIOD)
        {
            return None;
        }
        let quiet: Vec<String> = self.active.difference(&moving).cloned().collect();
        if moving.is_empty() && quiet.is_empty() {
            return None;
        }
        self.last = Some(now);
        self.active = moving.clone();
        Some(GateEmit {
            projects: moving.into_iter().collect(),
            quiet,
        })
    }
}

/// A node id as a device id string (the base64 used by holder rows and
/// membership snapshots).
pub fn device_id_of(node: &[u8; 32]) -> String {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    B64.encode(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn bytes_and_rate_accumulate_per_device_and_direction() {
        let m = ExchangeMeter::new();
        let t = t0();
        m.register("u1#1", FlowDirection::Recv, "p", "u1", "a.fits", 10_000_000);
        for i in 1..=10 {
            m.delivered(
                "u1#1",
                "DEV=",
                1_000_000,
                t + Duration::from_millis(500 * i),
            );
        }
        let v = m.snapshot(t + Duration::from_millis(5_000));
        assert_eq!(v.len(), 1);
        assert_eq!(
            (v[0].device.as_str(), v[0].direction, v[0].bytes_session),
            ("DEV=", FlowDirection::Recv, 10_000_000)
        );
        assert!(v[0].moving);
        assert!(
            (v[0].rate_bps - 2_000_000.0).abs() < 200_000.0,
            "≈2 MB/s, got {}",
            v[0].rate_bps
        );
        assert_eq!(
            v[0].in_flight,
            vec![InFlightView {
                frame_uuid: "u1".into(),
                file_name: "a.fits".into(),
                size: 10_000_000,
                done: 10_000_000
            }]
        );
    }

    #[test]
    fn a_quiet_flow_reads_zero_and_is_dropped_after_idle() {
        let m = ExchangeMeter::new();
        let t = t0();
        m.register("k", FlowDirection::Send, "p", "u", "f", 100);
        m.delivered("k", "D=", 100, t);
        m.finish("k", true, t);
        let v = m.snapshot(t + MOVING + Duration::from_millis(1));
        assert_eq!(
            (v[0].moving, v[0].rate_bps, v[0].completed),
            (false, 0.0, 1)
        );
        assert!(m
            .snapshot(t + IDLE_DROP + Duration::from_secs(1))
            .is_empty());
    }

    #[test]
    fn a_hedged_item_credits_both_and_completes_the_top_device() {
        let m = ExchangeMeter::new();
        let t = t0();
        m.register("k", FlowDirection::Recv, "p", "u", "f", 1000);
        m.delivered("k", "A=", 700, t);
        m.delivered("k", "B=", 400, t);
        assert_eq!(
            m.finish("k", true, t),
            vec![("A=".to_string(), 700), ("B=".to_string(), 400)]
        );
        let v = m.snapshot(t);
        let a = v.iter().find(|f| f.device == "A=").unwrap();
        let b = v.iter().find(|f| f.device == "B=").unwrap();
        assert_eq!(
            (a.bytes_session, a.completed, b.bytes_session, b.completed),
            (700, 1, 400, 0)
        );
        assert!(a.in_flight.is_empty() && b.in_flight.is_empty());
    }

    #[test]
    fn a_cancelled_item_leaves_nothing_in_flight() {
        let m = ExchangeMeter::new();
        let t = t0();
        m.register("k", FlowDirection::Recv, "p", "u", "f", 1000);
        m.delivered("k", "A=", 300, t);
        m.finish("k", false, t);
        let v = m.snapshot(t);
        assert_eq!((v[0].completed, v[0].in_flight.len()), (0, 0));
        assert!(!m.any_in_flight());
    }

    #[test]
    fn in_flight_done_is_clamped_to_a_known_size() {
        let m = ExchangeMeter::new();
        let t = t0();
        m.register("k", FlowDirection::Recv, "p", "u", "f", 1000);
        m.delivered("k", "A=", 700, t);
        // A retry re-delivering past what the known size expects (e.g. a
        // resend reusing the same key) must not read above 100%.
        m.delivered("k", "A=", 700, t);
        let v = m.snapshot(t);
        assert_eq!(v[0].in_flight[0].done, 1000);
    }

    #[test]
    fn one_uuid_in_two_projects_never_crosses() {
        let m = ExchangeMeter::new();
        let t = t0();
        m.register("u#1", FlowDirection::Recv, "p1", "u", "f", 10);
        m.register("u#2", FlowDirection::Recv, "p2", "u", "f", 10);
        m.delivered("u#1", "A=", 10, t);
        let v = m.snapshot(t);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].project_id, "p1");
    }

    #[test]
    fn an_unregistered_key_is_ignored() {
        let m = ExchangeMeter::new();
        m.delivered("nope", "A=", 10, t0());
        assert!(m.snapshot(t0()).is_empty());
    }

    #[test]
    fn eta_is_remaining_in_flight_over_rate() {
        let m = ExchangeMeter::new();
        let t = t0();
        m.register("k", FlowDirection::Recv, "p", "u", "f", 4_000_000);
        m.delivered("k", "A=", 1_000_000, t);
        m.delivered("k", "A=", 1_000_000, t + Duration::from_secs(1));
        let v = m.snapshot(t + Duration::from_secs(1));
        let eta = v[0].eta_secs.unwrap();
        assert!(eta > 0.5 && eta < 4.0, "2 MB left at ~1 MB/s: {eta}");
    }

    #[test]
    fn rate_reseeds_after_a_pause_instead_of_reading_the_stale_average() {
        let m = ExchangeMeter::new();
        let t = t0();
        m.register("k", FlowDirection::Recv, "p", "u", "f", 100_000_000);
        // A steady 2 MB/s at the SAMPLE granularity: 500,000 B every 250 ms
        // for 2 s.
        for i in 1..=8 {
            m.delivered("k", "A=", 500_000, t + Duration::from_millis(250 * i));
        }
        let before = m.snapshot(t + Duration::from_millis(2_000));
        assert!(
            (before[0].rate_bps - 2_000_000.0).abs() < 1.0,
            "steady state should read exactly 2 MB/s, got {}",
            before[0].rate_bps
        );

        // A 10 s pause (a stalled connection, a backoff retry, …), then the
        // exact same cadence resumes.
        let resume = t + Duration::from_millis(2_000) + Duration::from_secs(10);
        m.delivered("k", "A=", 500_000, resume);
        for i in 1..=4 {
            m.delivered("k", "A=", 500_000, resume + Duration::from_millis(250 * i));
        }
        // Checkpoint at 1s after resume, deliberately past the single
        // reseed delivery: the resumed delivery at `resume` only re-seeds
        // sample_at/sample_bytes/rate (the same rule as a brand-new flow),
        // and the very next 250 ms sample (at `resume + 250ms`) already
        // recomputes inst = 500,000 / 0.25s = 2,000,000 exactly — so by 1s
        // in (four steady post-pause samples deep) there is no EMA lag left
        // to wait out, and the rate has been exactly 2,000,000 for 750ms.
        let v = m.snapshot(resume + Duration::from_secs(1));
        assert!(v[0].moving);
        assert!(
            (v[0].rate_bps - 2_000_000.0).abs() < 200_000.0,
            "within 10% of 2 MB/s by 1s after resume, got {}",
            v[0].rate_bps
        );
    }

    #[test]
    fn the_gate_emits_at_most_once_per_period_and_once_more_when_quiet() {
        let mut g = ProgressGate::default();
        let t = t0();
        let p: std::collections::BTreeSet<String> = ["p".to_string()].into();
        assert_eq!(
            g.poll(t, p.clone()).unwrap().projects,
            vec!["p".to_string()]
        );
        assert!(g.poll(t + Duration::from_millis(500), p.clone()).is_none());
        assert!(g.armed());
        let quiet = g
            .poll(t + Duration::from_millis(1100), Default::default())
            .unwrap();
        assert_eq!(
            (quiet.projects.len(), quiet.quiet),
            (0, vec!["p".to_string()])
        );
        assert!(!g.armed());
        assert!(g
            .poll(t + Duration::from_secs(5), Default::default())
            .is_none());
    }

    #[test]
    fn device_id_is_padded_standard_base64() {
        assert_eq!(
            device_id_of(&[0u8; 32]),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        );
    }
    /// Task 12: a flow that starts moving wakes the runtime once (its
    /// continuing deltas do not); a progress event is owed while bytes are in
    /// flight or a flow moved within `MOVING`, and no longer after.
    #[tokio::test]
    async fn a_starting_flow_wakes_once_and_progress_is_owed_until_quiet() {
        let m = ExchangeMeter::new();
        let t = t0();
        let wait = || tokio::time::timeout(Duration::from_millis(100), m.flow_started());
        assert!(!m.needs_progress(t));
        m.register("u1#1", FlowDirection::Recv, "p", "u1", "a.fits", 100);
        m.delivered("u1#1", "DEV=", 10, t);
        assert!(wait().await.is_ok(), "a new flow wakes");
        m.delivered("u1#1", "DEV=", 10, t + Duration::from_millis(500));
        assert!(wait().await.is_err(), "a moving flow's next delta does not");
        assert!(
            m.needs_progress(t + Duration::from_secs(30)),
            "bytes in flight"
        );
        m.finish("u1#1", true, t + Duration::from_secs(1));
        assert!(m.needs_progress(t + Duration::from_secs(1) + MOVING));
        assert!(!m.needs_progress(t + Duration::from_secs(2) + MOVING));
        m.register("u2#2", FlowDirection::Recv, "p", "u2", "b.fits", 100);
        m.delivered("u2#2", "DEV=", 10, t + Duration::from_secs(10));
        assert!(wait().await.is_ok(), "a flow re-seeded after a pause wakes");
    }
}
