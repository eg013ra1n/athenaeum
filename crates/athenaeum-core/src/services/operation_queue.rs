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
    FileOpDelete,
}

impl OperationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationKind::ZipArchive => "zip_archive",
            OperationKind::FileOpMove => "file_op_move",
            OperationKind::FileOpDelete => "file_op_delete",
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

/// Serializable summary used by inspection/listing APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub kind: OperationKind,
    pub operation_id: i64,
}

struct Inner {
    pending: Mutex<VecDeque<QueuedJob>>,
    notify: Condvar,
    /// Snapshot for inspection. Updated under the same mutex as `pending`.
    inspector: Mutex<Vec<QueueEntry>>,
    /// Currently running entry, if any. Updated by the worker.
    running: Mutex<Option<QueueEntry>>,
}

#[derive(Clone)]
pub struct OperationQueue {
    inner: Arc<Inner>,
}

impl OperationQueue {
    /// Construct a queue and spawn its single worker thread. The worker
    /// runs forever until the process exits.
    pub fn start() -> Self {
        let inner = Arc::new(Inner {
            pending: Mutex::new(VecDeque::new()),
            notify: Condvar::new(),
            inspector: Mutex::new(Vec::new()),
            running: Mutex::new(None),
        });
        let queue = OperationQueue { inner: inner.clone() };
        thread::Builder::new()
            .name("athenaeum-op-queue".into())
            .spawn(move || worker_loop(inner))
            .expect("failed to spawn operation queue worker");
        queue
    }

    /// Push a job onto the back of the queue. Returns immediately.
    pub fn enqueue(&self, job: QueuedJob) {
        let entry = QueueEntry {
            kind: job.kind,
            operation_id: job.operation_id,
        };
        {
            let mut pending = self.inner.pending.lock().unwrap();
            let mut inspector = self.inner.inspector.lock().unwrap();
            pending.push_back(job);
            inspector.push(entry);
        }
        self.inner.notify.notify_one();
    }

    /// Snapshot of queued jobs (front-to-back order). Does not include the
    /// currently-running job.
    pub fn pending_snapshot(&self) -> Vec<QueueEntry> {
        self.inner.inspector.lock().unwrap().clone()
    }

    /// Currently-running job, if any.
    pub fn running_snapshot(&self) -> Option<QueueEntry> {
        self.inner.running.lock().unwrap().clone()
    }
}

fn worker_loop(inner: Arc<Inner>) {
    loop {
        // Wait for a job.
        let job = {
            let mut pending = inner.pending.lock().unwrap();
            while pending.is_empty() {
                pending = inner.notify.wait(pending).unwrap();
            }
            // Pop from both pending and the inspector snapshot.
            let job = pending.pop_front().unwrap();
            let mut inspector = inner.inspector.lock().unwrap();
            if !inspector.is_empty() {
                inspector.remove(0);
            }
            job
        };

        // Mark running.
        {
            let mut running = inner.running.lock().unwrap();
            *running = Some(QueueEntry {
                kind: job.kind,
                operation_id: job.operation_id,
            });
        }

        // Run. Catch panics so a misbehaving job doesn't kill the worker.
        let job_kind = job.kind;
        let op_id = job.operation_id;
        let run_fn = job.run;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run_fn));
        if let Err(_panic) = result {
            eprintln!(
                "operation_queue: job kind={} op_id={} panicked",
                job_kind.as_str(),
                op_id
            );
        }

        // Clear running.
        {
            let mut running = inner.running.lock().unwrap();
            *running = None;
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
}
