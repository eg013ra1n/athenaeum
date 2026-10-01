//! Single-worker serialized queue for long-running operations.
//!
//! Both ZIP archive and file-op (Move/Delete) jobs go through this queue so
//! they don't compete for disk bandwidth or step on each other's catalog
//! transactions. Only one job runs at a time across both kinds.
//!
//! The queue is intentionally thin: it holds boxed closures plus enough
//! metadata to identify the job. Each enqueue site builds the closure with
//! its own captured emitter, connection-acquirer, and bookkeeping, so the
//! queue itself doesn't need to know about Tauri events or rusqlite.
//!
//! Cancellation today is just "set the cancel flag the closure already
//! owns." If the job is still queued (not yet running), the worker will
//! reach it eventually and the executor will see the flag and short-circuit.
//! A future refinement can pop already-cancelled jobs without running them.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

/// Distinguishes job kinds for telemetry and inspection. The closure itself
/// is what dispatches to the right executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    ZipArchive,
    FileOpMove,
    /// Auto-reconcile abandoned cross-volume Move commits
    /// (`file_op::reconcile::reconcile_abandoned_commit_moves`). Enqueued
    /// once at startup and does not correspond to a `file_operations` row —
    /// its `operation_id` is always 0.
    FileOpReconcile,
}

impl OperationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationKind::ZipArchive => "zip_archive",
            OperationKind::FileOpMove => "file_op_move",
            OperationKind::FileOpReconcile => "file_op_reconcile",
        }
    }
}

/// One unit of work pushed onto the queue.
pub struct QueuedJob {
    pub kind: OperationKind,
    pub operation_id: i64,
    /// The actual work to do. Called from the queue worker thread; takes no
    /// arguments because it captures whatever it needs (DB context, emitter,
    /// cancel flag) at enqueue time.
    pub run: Box<dyn FnOnce() + Send + 'static>,
}

struct Inner {
    pending: Mutex<Pending>,
    notify: Condvar,
}

struct Pending {
    jobs: VecDeque<QueuedJob>,
    /// Set when the last [`OperationQueue`] handle is dropped: the worker
    /// drains what is queued, then exits.
    closed: bool,
}

/// The one owner every [`OperationQueue`] clone shares; dropping it closes
/// the queue. Separate from [`Inner`], which the worker holds, so the
/// worker's own reference never keeps the queue open.
struct Handle {
    inner: Arc<Inner>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Under the lock, so a worker between its empty check and its wait
        // cannot miss the wake-up.
        let mut pending = self.inner.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending.closed = true;
        self.inner.notify.notify_all();
    }
}

#[derive(Clone)]
pub struct OperationQueue {
    handle: Arc<Handle>,
}

impl OperationQueue {
    /// Construct a queue and spawn its single worker thread. The worker runs
    /// for as long as any handle to the queue is alive (the app's own
    /// `ServiceContext` keeps one for the process lifetime); once the last
    /// handle is dropped it finishes the queued jobs and exits.
    pub fn start() -> Self {
        Self::spawn().0
    }

    fn spawn() -> (Self, thread::JoinHandle<()>) {
        let inner = Arc::new(Inner {
            pending: Mutex::new(Pending {
                jobs: VecDeque::new(),
                closed: false,
            }),
            notify: Condvar::new(),
        });
        let queue = OperationQueue {
            handle: Arc::new(Handle {
                inner: inner.clone(),
            }),
        };
        let worker = thread::Builder::new()
            .name("athenaeum-op-queue".into())
            .spawn(move || worker_loop(inner))
            .expect("failed to spawn operation queue worker");
        (queue, worker)
    }

    /// Push a job onto the back of the queue. Returns immediately.
    pub fn enqueue(&self, job: QueuedJob) {
        let inner = &self.handle.inner;
        {
            let mut pending = inner.pending.lock().unwrap();
            pending.jobs.push_back(job);
        }
        inner.notify.notify_one();
    }
}

fn worker_loop(inner: Arc<Inner>) {
    loop {
        // Wait for a job; with the queue closed and drained, stop.
        let job = {
            let mut pending = inner.pending.lock().unwrap();
            loop {
                if let Some(job) = pending.jobs.pop_front() {
                    break job;
                }
                if pending.closed {
                    tracing::debug!("operation queue closed; worker exiting");
                    return;
                }
                pending = inner.notify.wait(pending).unwrap();
            }
        };

        // Run. Catch panics so a misbehaving job doesn't kill the worker.
        let job_kind = job.kind;
        let op_id = job.operation_id;
        let run_fn = job.run;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run_fn));
        if let Err(_panic) = result {
            tracing::error!(
                kind = job_kind.as_str(),
                operation_id = op_id,
                "operation queue job panicked"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn jobs_run_serially_in_order() {
        let queue = OperationQueue::start();
        let counter = Arc::new(AtomicUsize::new(0));
        let log = Arc::new(Mutex::new(Vec::<i64>::new()));

        for op_id in 0..10 {
            let counter = counter.clone();
            let log = log.clone();
            queue.enqueue(QueuedJob {
                kind: OperationKind::ZipArchive,
                operation_id: op_id,
                run: Box::new(move || {
                    let prev = counter.fetch_add(1, Ordering::SeqCst);
                    // If serial, prev should always equal op_id since we
                    // enqueued in 0..10 order and pop is FIFO.
                    log.lock().unwrap().push(op_id);
                    // Sleep briefly to magnify any concurrency bug.
                    thread::sleep(Duration::from_millis(2));
                    assert_eq!(prev as i64, op_id, "jobs ran out of order");
                }),
            });
        }

        // Wait for completion.
        for _ in 0..200 {
            if counter.load(Ordering::SeqCst) == 10 {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(counter.load(Ordering::SeqCst), 10);
        assert_eq!(*log.lock().unwrap(), (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn worker_survives_panicking_job() {
        let queue = OperationQueue::start();
        let after = Arc::new(AtomicUsize::new(0));

        // First job panics.
        queue.enqueue(QueuedJob {
            kind: OperationKind::FileOpMove,
            operation_id: 1,
            run: Box::new(|| {
                panic!("simulated failure");
            }),
        });
        // Second job should still run.
        let after2 = after.clone();
        queue.enqueue(QueuedJob {
            kind: OperationKind::FileOpMove,
            operation_id: 2,
            run: Box::new(move || {
                after2.fetch_add(1, Ordering::SeqCst);
            }),
        });

        for _ in 0..100 {
            if after.load(Ordering::SeqCst) == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(after.load(Ordering::SeqCst), 1);
    }

    /// The worker ends once the last handle is dropped — after running every
    /// job queued before that — and not while any handle is alive.
    #[test]
    fn worker_drains_and_exits_after_the_last_handle_drops() {
        let (queue, worker) = OperationQueue::spawn();
        let other = queue.clone();
        drop(other);
        thread::sleep(Duration::from_millis(50));
        assert!(
            !worker.is_finished(),
            "an idle worker stays while a handle is alive"
        );

        let ran = Arc::new(AtomicUsize::new(0));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        // The first job holds the worker until the queue is closed, so the
        // second is still queued then.
        queue.enqueue(QueuedJob {
            kind: OperationKind::ZipArchive,
            operation_id: 1,
            run: Box::new(move || {
                gate_rx.recv().ok();
            }),
        });
        let ran2 = ran.clone();
        queue.enqueue(QueuedJob {
            kind: OperationKind::ZipArchive,
            operation_id: 2,
            run: Box::new(move || {
                ran2.fetch_add(1, Ordering::SeqCst);
            }),
        });
        drop(queue);
        gate_tx.send(()).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        thread::spawn(move || done_tx.send(worker.join().is_ok()).ok());
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "the worker thread ended"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1, "the queued job still ran");
    }
}
