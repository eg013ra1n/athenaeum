//! The live collab exchange (wave 3): the hub event stream, holder/claim
//! bookkeeping, and everything that keeps a device's copy of a project in
//! sync while it is running. Retry back-off and the claim digest, the REST
//! wire shapes and event-stream types, the SSE parser and stream reader, and
//! the presence book land here, and so do the holder map with its derived
//! providers and the outbox report building (Task 6); later tasks in this
//! wave append the local session.

pub mod backoff;
pub mod cursor;
pub mod digest;
pub mod holders;
pub mod meter;
pub mod outbox;
pub mod presence;
pub mod sse;
pub mod stream;
pub mod wire;
