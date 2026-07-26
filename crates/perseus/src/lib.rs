//! Perseus — the headless capture-node agent.
//!
//! Perseus watches an astrophotography **capture directory**, waits for each new
//! FITS/XISF sub-exposure to finish writing (write-stability debounce), wraps it
//! in a portable [`athenaeum_core::package`], and hands it to the
//! [`athenaeum_core::sync`] engine to deliver to a paired primary over the
//! [`athenaeum_core::sharing::iroh`] transport. No catalog, no UI — a systemd /
//! launchd service on the capture machine.
//!
//! The crate is a library + binary: the library exposes the agent internals so
//! the loopback e2e test (a separate crate) can inject an in-process transport.
//!
//! # Module map
//!
//! - [`config`] — the `perseus.toml` contract: parse + actionable validation.
//! - [`config_edit`] — comment-preserving `[retention]` write-back for the web
//!   settings page (`toml_edit`), re-validated before an atomic replace.
//! - [`account`] — hub account pairing (task M1): the `perseus login` OTP flow,
//!   the pairing cache, and the run-time peer/relay resolution.
//! - [`watcher`] — `notify` watcher + the pure, clock-injectable
//!   [`watcher::StabilityTracker`].
//! - [`run`] — the [`run::Agent`]: store + transport + engine + watcher, plus
//!   package building, the device key, and logging setup.
//! - [`batcher`] — the send batcher ([`batcher::spawn_batcher`]): accumulates the
//!   watcher's stable files and flushes them as one package on the auto
//!   quiet-timer or a manual signal, replacing the old per-file consumer.
//! - [`supervisor`] — the readiness-driven engine lifecycle: a state machine
//!   that launches/stops the [`run::Agent`] as the config gains or loses
//!   readiness (signed in + ≥1 capture dir), with a launcher seam for tests.
//! - [`seen`] — durable, stat-aware "already enqueued this exact file" dedup
//!   (`perseus_seen` table), so a restart never re-baselines an un-synced
//!   frame into oblivion.
//! - [`web`] — the embedded status-page server: an axum router with bearer auth
//!   and read-only status/sent/history endpoints (task 9); Task 10 adds write
//!   handlers onto the same router.
//! - [`library`] — the library path contract: wire rel-paths are forward-slash,
//!   and [`library::resolve_in_root`] is the single containment guard every
//!   library route (listing, preview, send, delete) resolves user paths through.
//! - [`pending`] — pure derivation of the "To sync" tree: groups the batcher's
//!   pending accumulator snapshot into a [`pending::PendingNode`] trie by
//!   `rel_path` (object / date / type / file) for the web view.

pub mod account;
pub mod batch_store;
pub mod batcher;
pub mod config;
pub mod config_edit;
pub mod library;
pub mod pending;
pub mod resend;
pub mod run;
pub mod seen;
pub mod supervisor;
#[cfg(feature = "tray")]
pub mod tray;
pub mod watcher;
pub mod web;

pub use config::Config;
pub use run::Agent;
