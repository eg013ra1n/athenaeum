//! The live collab exchange (wave 3): the hub event stream, holder/claim
//! bookkeeping, and everything that keeps a device's copy of a project in
//! sync while it is running. This task lands the pieces the hub client needs
//! — retry back-off and the claim digest — plus the REST wire shapes; later
//! tasks in this wave append the event-stream types and the local session.

pub mod backoff;
pub mod digest;
pub mod wire;
