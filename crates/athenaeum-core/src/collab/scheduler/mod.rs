//! The receive-side collab scheduler (spec §7): a pure, deterministic core
//! (`core`) that the live executor (`api::collab_live`, Task 15) steps with
//! hub, disk, fetch and timer inputs and whose commands it performs. The
//! seeded simulation (`sim_tests`) drives the core against the production
//! provider derivation, presence book, feed cursor and serve decision and
//! asserts the live-exchange invariants after every step (spec §12).

pub mod core;
#[cfg(test)]
mod sim_tests;
