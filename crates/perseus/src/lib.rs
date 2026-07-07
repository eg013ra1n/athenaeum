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
//! - [`watcher`] — `notify` watcher + the pure, clock-injectable
//!   [`watcher::StabilityTracker`].
//! - [`run`] — the [`run::Agent`]: store + transport + engine + watcher, plus
//!   package building, the device key, and logging setup.

pub mod config;
pub mod run;
pub mod watcher;

pub use config::Config;
pub use run::Agent;
