//! Per-frame cooperative deadline, shared by desktop and web solvers.
//! The watchdog only requests cancellation: the worker stays joined until the
//! solver exits, so timed-out work never accumulates in detached threads.
use anyhow::Result;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum Interrupted {
    Timeout(u32),
    Cancelled,
}

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout(s) => write!(
                f,
                "Plate solve exceeded the {s} second per-frame time limit"
            ),
            Self::Cancelled => write!(f, "Plate solve cancelled"),
        }
    }
}
impl std::error::Error for Interrupted {}

struct Finished(mpsc::Sender<()>);
impl Drop for Finished {
    fn drop(&mut self) {
        // A disconnected receiver means the watchdog already stopped. Drop must
        // also release a waiting watchdog when the worker exits through a panic.
        let _ = self.0.send(());
    }
}

/// Includes image reading and all fallback attempts. Blocking I/O or a long
/// individual solver operation can overrun until its next cancellation check.
///
/// `seconds = 0` disables the deadline. `cancel` is the optional shared batch
/// flag; `work` receives a separate flag that it must poll cooperatively. This
/// function never sets the batch flag. It returns the work result unless batch
/// cancellation or the deadline wins; cancellation takes precedence over timeout.
pub fn run<T>(
    seconds: u32,
    cancel: Option<&AtomicBool>,
    work: impl FnOnce(&AtomicBool) -> Result<T>,
) -> Result<T> {
    run_for(Duration::from_secs(seconds.into()), seconds, cancel, work)
}
fn run_for<T>(
    limit: Duration,
    seconds: u32,
    cancel: Option<&AtomicBool>,
    work: impl FnOnce(&AtomicBool) -> Result<T>,
) -> Result<T> {
    let local = AtomicBool::new(false);
    let timed_out = AtomicBool::new(false);
    let start = Instant::now();
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let local_ref = &local;
        let timed_out_ref = &timed_out;
        scope.spawn(move || loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                local_ref.store(true, Ordering::Relaxed);
                break;
            }
            if !limit.is_zero() && start.elapsed() >= limit {
                timed_out_ref.store(true, Ordering::Relaxed);
                local_ref.store(true, Ordering::Relaxed);
                break;
            }
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        });
        let finished = Finished(tx);
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(Interrupted::Cancelled.into());
        }
        let result = work(&local);
        drop(finished);
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(Interrupted::Cancelled.into());
        }
        if timed_out.load(Ordering::Relaxed) || (!limit.is_zero() && start.elapsed() >= limit) {
            return Err(Interrupted::Timeout(seconds).into());
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_stops_one_frame_and_next_frame_can_run() {
        let parent = AtomicBool::new(false);
        let result: Result<()> = run_for(Duration::from_millis(25), 1, Some(&parent), |flag| {
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            anyhow::bail!("solver cancelled")
        });
        assert!(matches!(
            result.unwrap_err().downcast_ref::<Interrupted>(),
            Some(Interrupted::Timeout(1))
        ));
        assert!(!parent.load(Ordering::Relaxed));
        assert_eq!(run(1, Some(&parent), |_| Ok(42)).unwrap(), 42);
    }
    #[test]
    fn batch_cancel_and_fast_completion_are_distinct_from_timeout() {
        let parent = AtomicBool::new(true);
        let result = run(1, Some(&parent), |_| Ok(42)).unwrap_err();
        assert!(matches!(
            result.downcast_ref::<Interrupted>(),
            Some(Interrupted::Cancelled)
        ));
        assert_eq!(run(0, None, |_| Ok(42)).unwrap(), 42);
    }
    #[test]
    fn panic_does_not_leave_watchdog_waiting() {
        assert!(std::panic::catch_unwind(|| run::<()>(3600, None, |_| panic!("fixture"))).is_err());
    }
}
