pub mod config;
pub mod dso_lookup;
pub mod failure;
pub mod gate_audit;
pub mod hints;
pub mod layers;
pub mod object_fill;
pub mod service;
pub mod storage;

pub use config::PlateSolveConfig;
pub use failure::{describe_solve_failure, SolveFailureInfo};
pub use layers::discover_layers;
pub use service::{
    solve_frame, solve_frame_layered, solve_frame_with_hints, store_result, SolveResult,
    StoreOutcome,
};

pub use astroimage::platesolving::SolveHints;
pub use storage::{
    delete_plate_solve, get_plate_solve, insert_plate_solve, update_frame_from_solve,
    PlateSolveRecord,
};
