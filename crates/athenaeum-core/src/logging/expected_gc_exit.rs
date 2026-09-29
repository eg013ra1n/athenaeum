//! Drops the one `ERROR error during gc run` iroh-blobs emits for every blob
//! store we close on purpose.
//!
//! iroh-blobs 0.103's GC task ends only when a run fails, and logs that
//! failure at `error!` (`store::gc::run_gc`). `StoreGc::close`
//! (`sharing::iroh::node`) ends the task exactly that way — its released run
//! meets the already shut store — so every app exit and every collab store
//! swap printed an ERROR that is really the expected end of the task. Each
//! close announces the one failure it causes via [`expect_blob_gc_exit`];
//! this layer swallows that many matching events and no more, so a GC run
//! that fails on its own (the store still open) stays an ERROR.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

const GC_TARGET: &str = "iroh_blobs::store::gc";
const GC_RUN_FAILED: &str = "error during gc run";

static PENDING: AtomicUsize = AtomicUsize::new(0);

/// Announce that a blob store's GC task is about to end through a failed run
/// (called once per `StoreGc::close`).
pub(crate) fn expect_blob_gc_exit() {
    PENDING.fetch_add(1, Ordering::SeqCst);
    tracing::debug!("blob store gc closing");
}

pub(super) struct ExpectedGcExit {
    pending: &'static AtomicUsize,
}

impl ExpectedGcExit {
    pub(super) fn new() -> Self {
        Self { pending: &PENDING }
    }
}

impl<S: Subscriber> Layer<S> for ExpectedGcExit {
    fn event_enabled(&self, event: &Event<'_>, _ctx: Context<'_, S>) -> bool {
        let meta = event.metadata();
        if meta.target() != GC_TARGET || *meta.level() != Level::ERROR {
            return true;
        }
        let mut msg = MessageStart::default();
        event.record(&mut msg);
        if !msg.gc_run_failed {
            return true;
        }
        // Take one expected exit if any is owed; otherwise a real failure.
        self.pending
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_err()
    }
}

#[derive(Default)]
struct MessageStart {
    gc_run_failed: bool,
}

impl Visit for MessageStart {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.gc_run_failed = format!("{value:?}").starts_with(GC_RUN_FAILED);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone, Default)]
    struct Seen(Arc<Mutex<Vec<String>>>);

    impl<S: Subscriber> Layer<S> for Seen {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            #[derive(Default)]
            struct Msg(String);
            impl Visit for Msg {
                fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut m = Msg::default();
            event.record(&mut m);
            self.0.lock().unwrap().push(m.0);
        }
    }

    #[test]
    fn swallows_only_the_announced_gc_exits() {
        // Own counter: the process-global one is bumped by parallel iroh tests.
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let seen = Seen::default();
        let subscriber = tracing_subscriber::registry()
            .with(ExpectedGcExit { pending: &COUNTER })
            .with(seen.clone());
        tracing::subscriber::with_default(subscriber, || {
            COUNTER.fetch_add(1, Ordering::SeqCst);
            tracing::error!(target: "iroh_blobs::store::gc", "error during gc run: {}", "Error::Io");
            // Nothing owed any more: a second failure is real and stays visible.
            tracing::error!(target: "iroh_blobs::store::gc", "error during gc run: {}", "Error::Io");
            // Other gc errors and other targets are never touched.
            COUNTER.fetch_add(1, Ordering::SeqCst);
            tracing::error!(target: "iroh_blobs::store::gc", "error during gc sweep: x");
            tracing::error!(target: "other", "error during gc run: y");
        });
        assert_eq!(
            *seen.0.lock().unwrap(),
            vec![
                "error during gc run: Error::Io".to_string(),
                "error during gc sweep: x".to_string(),
                "error during gc run: y".to_string(),
            ]
        );
        assert_eq!(
            COUNTER.load(Ordering::SeqCst),
            1,
            "unmatched events never consume"
        );
    }
}
