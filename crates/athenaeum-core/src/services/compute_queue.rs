//! Global FIFO admission queue for heavy CPU jobs (analysis, master builds,
//! light calibration). One heavy job at a time by default
//! (`compute.max_concurrent`), so an analysis started in another tab queues
//! behind a running master build instead of fighting it for the rayon pool.
//!
//! The queue is an admission controller, not a job runner: `acquire()` blocks
//! the calling thread (each caller already sits on its own
//! `spawn_blocking`/`std::thread`) until a slot frees AND every
//! earlier-enqueued ticket has been admitted. The returned permit frees the
//! slot on Drop. Cancellation of a QUEUED job flips the same cancel flag the
//! running job would poll; the waiting `acquire` sees it and returns
//! `Err(QueueCancelled)` without ever running.
//!
//! # Locking
//!
//! Lock hierarchy (outermost first): `gate` → `waiting` → `registry`. Every
//! waking state transition (admit, permit release, queued-cancel removal,
//! cancel-flag set, max_concurrent change) mutates under `gate` and calls
//! `cv.notify_all()` before releasing it, so a waiter parked in `acquire`'s
//! wait loop can never miss a wakeup: the waiter only evaluates its admission
//! condition and parks while holding `gate`, and `Condvar::wait_timeout`
//! releases `gate` atomically. The 200 ms `wait_timeout` therefore remains
//! ONLY as a defensive backstop — the common path (finish → next admit,
//! cancel, concurrency change) is wake-on-notify.
//!
//! Enqueue is the one transition taken WITHOUT `gate`: pushing a ticket onto
//! the back can never make another waiter admittable, and the enqueuing
//! thread re-evaluates its own condition after taking `gate`. It drops
//! `waiting`/`registry` before the wait loop takes `gate`, consistent with
//! the hierarchy (never acquire `gate` while holding an inner lock).
//!
//! The transport notifier is always invoked OUTSIDE all queue locks; a
//! panicking callback is caught and logged rather than poisoning the queue
//! (notification also runs inside `ComputePermit::drop`). The callback must
//! not call back into `set_notifier`.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ComputeJobKind {
    Analysis,
    MasterBuild,
    LightCalibration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ComputeJobState {
    Queued,
    Running,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ComputeQueueEntry {
    pub job_id: i64,
    pub kind: ComputeJobKind,
    pub label: String,
    pub state: ComputeJobState,
    pub queued_at: String,
}

/// Job was cancelled while still waiting in the queue.
#[derive(Debug)]
pub struct QueueCancelled;

struct JobSlot {
    entry: ComputeQueueEntry,
    cancel_flag: Arc<AtomicBool>,
}

struct Inner {
    /// Tickets in FIFO order. Front = next to admit. Running jobs are NOT in
    /// this deque — they live only in `registry`.
    waiting: Mutex<VecDeque<i64>>,
    registry: Mutex<Vec<JobSlot>>,
    running_count: AtomicUsize,
    max_concurrent: AtomicUsize,
    next_id: AtomicI64,
    cv: Condvar,
    /// Guards cv waits AND every waking state transition (see the module
    /// doc's lock hierarchy); the actual state lives in
    /// waiting/registry/counters.
    gate: Mutex<()>,
    /// `Arc` (not `Box`) so `notify()` can clone the callback under the lock
    /// and invoke it after releasing it — outside all queue locks.
    notifier: Mutex<Option<Arc<dyn Fn(Vec<ComputeQueueEntry>) + Send + Sync>>>,
}

#[derive(Clone)]
pub struct ComputeQueue {
    inner: Arc<Inner>,
}

impl ComputeQueue {
    pub fn new() -> Self {
        ComputeQueue {
            inner: Arc::new(Inner {
                waiting: Mutex::new(VecDeque::new()),
                registry: Mutex::new(Vec::new()),
                running_count: AtomicUsize::new(0),
                max_concurrent: AtomicUsize::new(1),
                next_id: AtomicI64::new(1),
                cv: Condvar::new(),
                gate: Mutex::new(()),
                notifier: Mutex::new(None),
            }),
        }
    }

    pub fn set_max_concurrent(&self, n: usize) {
        // Mutate + notify under `gate` so a waiter can't check-then-park
        // between the store and the wakeup (lost-wakeup race).
        let gate = self.inner.gate.lock().unwrap();
        self.inner.max_concurrent.store(n.max(1), Ordering::SeqCst);
        self.inner.cv.notify_all();
        drop(gate);
    }

    pub fn max_concurrent(&self) -> usize {
        self.inner.max_concurrent.load(Ordering::SeqCst)
    }

    pub fn set_notifier(&self, f: Box<dyn Fn(Vec<ComputeQueueEntry>) + Send + Sync>) {
        *self.inner.notifier.lock().unwrap() = Some(Arc::from(f));
    }

    /// Invoke the transport notifier with a fresh snapshot. Always called
    /// OUTSIDE all queue locks (the callback is cloned out of the notifier
    /// lock before invocation). A panicking callback is caught and logged so
    /// it can never poison the queue's mutexes or abort the process by
    /// panicking inside `ComputePermit::drop`.
    fn notify(&self) {
        let snap = self.snapshot();
        let cb = self.inner.notifier.lock().unwrap().clone();
        if let Some(f) = cb {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(snap))).is_err() {
                tracing::error!("compute queue notifier callback panicked");
            }
        }
    }

    pub fn snapshot(&self) -> Vec<ComputeQueueEntry> {
        self.inner.registry.lock().unwrap().iter().map(|s| s.entry.clone()).collect()
    }

    /// Sets the job's cancel flag (queued or running). Returns false if the
    /// id is unknown. For a QUEUED job the registry removal itself happens in
    /// the waiting thread's cancellation branch (which notifies again with
    /// the drained snapshot); the notification here tells the transport that
    /// a cancellation was requested.
    pub fn cancel(&self, job_id: i64) -> bool {
        let gate = self.inner.gate.lock().unwrap();
        let found = {
            let registry = self.inner.registry.lock().unwrap();
            match registry.iter().find(|s| s.entry.job_id == job_id) {
                Some(slot) => {
                    slot.cancel_flag.store(true, Ordering::SeqCst);
                    true
                }
                None => false,
            }
        };
        if found {
            self.inner.cv.notify_all();
        }
        drop(gate);
        if found {
            self.notify();
        }
        found
    }

    pub fn acquire(
        &self,
        kind: ComputeJobKind,
        label: &str,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<(ComputePermit, i64), QueueCancelled> {
        let job_id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut waiting = self.inner.waiting.lock().unwrap();
            let mut registry = self.inner.registry.lock().unwrap();
            waiting.push_back(job_id);
            registry.push(JobSlot {
                entry: ComputeQueueEntry {
                    job_id,
                    kind,
                    label: label.to_string(),
                    state: ComputeJobState::Queued,
                    queued_at: chrono::Utc::now().to_rfc3339(),
                },
                cancel_flag: cancel_flag.clone(),
            });
        }
        self.notify();

        // Wait until: cancelled, or (front of queue AND slot free).
        let mut gate = self.inner.gate.lock().unwrap();
        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                // Removing this ticket may make the next waiter front-of-queue,
                // so mutate + notify under `gate` (hierarchy: gate → waiting
                // → registry), then run the transport callback lock-free.
                {
                    let mut waiting = self.inner.waiting.lock().unwrap();
                    waiting.retain(|&id| id != job_id);
                    let mut registry = self.inner.registry.lock().unwrap();
                    registry.retain(|s| s.entry.job_id != job_id);
                }
                self.inner.cv.notify_all();
                drop(gate);
                self.notify();
                return Err(QueueCancelled);
            }
            let is_front = {
                let waiting = self.inner.waiting.lock().unwrap();
                waiting.front() == Some(&job_id)
            };
            let free = self.inner.running_count.load(Ordering::SeqCst)
                < self.inner.max_concurrent.load(Ordering::SeqCst);
            if is_front && free {
                break;
            }
            let (g, _timeout) = self
                .inner
                .cv
                .wait_timeout(gate, std::time::Duration::from_millis(200))
                .unwrap();
            gate = g;
        }

        // Admit: pop ticket, bump running, flip registry state — all under
        // `gate`, notify before releasing it (see module-doc lock hierarchy).
        {
            let mut waiting = self.inner.waiting.lock().unwrap();
            waiting.pop_front();
        }
        self.inner.running_count.fetch_add(1, Ordering::SeqCst);
        {
            let mut registry = self.inner.registry.lock().unwrap();
            if let Some(slot) = registry.iter_mut().find(|s| s.entry.job_id == job_id) {
                slot.entry.state = ComputeJobState::Running;
            }
        }
        self.inner.cv.notify_all();
        drop(gate);
        self.notify();

        Ok((ComputePermit { queue: self.clone(), job_id }, job_id))
    }
}

impl Default for ComputeQueue {
    fn default() -> Self { Self::new() }
}

/// RAII slot: releasing (Drop) frees the concurrency slot, removes the
/// registry entry, wakes waiters, and notifies the transport.
pub struct ComputePermit {
    queue: ComputeQueue,
    job_id: i64,
}

impl Drop for ComputePermit {
    fn drop(&mut self) {
        // Mutate + notify under `gate` so the finish → next-admit handoff is
        // wake-on-notify (a notify outside `gate` could land between a
        // waiter's condition check and its park, and be lost). The transport
        // callback runs after releasing all queue locks.
        let gate = self.queue.inner.gate.lock().unwrap();
        self.queue.inner.running_count.fetch_sub(1, Ordering::SeqCst);
        {
            let mut registry = self.queue.inner.registry.lock().unwrap();
            registry.retain(|s| s.entry.job_id != self.job_id);
        }
        self.queue.inner.cv.notify_all();
        drop(gate);
        self.queue.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn flag() -> Arc<AtomicBool> { Arc::new(AtomicBool::new(false)) }

    #[test]
    fn fifo_one_at_a_time() {
        let q = ComputeQueue::new();
        q.set_max_concurrent(1);
        let running = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let mut handles = Vec::new();
        for i in 0..5 {
            let (q, running, max_seen, order) = (q.clone(), running.clone(), max_seen.clone(), order.clone());
            handles.push(std::thread::spawn(move || {
                // stagger enqueue so ticket order is deterministic
                std::thread::sleep(Duration::from_millis(i as u64 * 30));
                let (_permit, _id) = q.acquire(ComputeJobKind::Analysis, &format!("job{i}"), flag()).unwrap();
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                order.lock().unwrap().push(i);
                std::thread::sleep(Duration::from_millis(50));
                running.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles { h.join().unwrap(); }
        assert_eq!(max_seen.load(Ordering::SeqCst), 1, "only one job may run");
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3, 4], "FIFO admission");
    }

    #[test]
    fn cancel_while_queued_returns_err_and_frees_ticket() {
        let q = ComputeQueue::new();
        q.set_max_concurrent(1);
        let hold = flag();
        let q2 = q.clone();
        let hold2 = hold.clone();
        let first = std::thread::spawn(move || {
            let (_p, _id) = q2.acquire(ComputeJobKind::Analysis, "long", hold2).unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });
        std::thread::sleep(Duration::from_millis(50));
        // Second job queues behind first; cancel it while it waits.
        let cancelled = flag();
        let q3 = q.clone();
        let c2 = cancelled.clone();
        let second = std::thread::spawn(move || q3.acquire(ComputeJobKind::MasterBuild, "victim", c2));
        std::thread::sleep(Duration::from_millis(50));
        let snap = q.snapshot();
        let victim = snap.iter().find(|e| e.label == "victim").expect("queued entry visible");
        assert!(matches!(victim.state, ComputeJobState::Queued));
        assert!(q.cancel(victim.job_id));
        let res = second.join().unwrap();
        assert!(res.is_err(), "cancelled-in-queue must not be admitted");
        first.join().unwrap();
        assert!(q.snapshot().is_empty(), "registry drained");
    }

    #[test]
    fn concurrency_two_admits_two() {
        let q = ComputeQueue::new();
        q.set_max_concurrent(2);
        let running = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let (q, running, max_seen) = (q.clone(), running.clone(), max_seen.clone());
            handles.push(std::thread::spawn(move || {
                let (_p, _id) = q.acquire(ComputeJobKind::Analysis, "j", flag()).unwrap();
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(60));
                running.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles { h.join().unwrap(); }
        assert_eq!(max_seen.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn notifier_fires_on_transitions() {
        let q = ComputeQueue::new();
        q.set_max_concurrent(1);
        let calls = Arc::new(AtomicUsize::new(0));
        let snaps = Arc::new(std::sync::Mutex::new(Vec::<Vec<ComputeQueueEntry>>::new()));
        let c2 = calls.clone();
        let s2 = snaps.clone();
        q.set_notifier(Box::new(move |snap| {
            c2.fetch_add(1, Ordering::SeqCst);
            s2.lock().unwrap().push(snap);
        }));
        let (p, id) = q.acquire(ComputeJobKind::Analysis, "n", flag()).unwrap();
        drop(p);
        // enqueue -> running -> finished: at least 2 notifications
        assert!(calls.load(Ordering::SeqCst) >= 2);
        let snaps = snaps.lock().unwrap();
        assert!(
            snaps.iter().any(|s| s
                .iter()
                .any(|e| e.job_id == id && matches!(e.state, ComputeJobState::Running))),
            "some snapshot must show the job in Running state"
        );
        assert!(
            snaps.last().expect("at least one snapshot").is_empty(),
            "final snapshot after permit drop must be empty"
        );
    }
}
