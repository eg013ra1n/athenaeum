pub mod astap;
pub mod config;
pub mod dso_lookup;
pub mod gate_audit;
pub mod hints;
pub mod index_builder;
pub mod object_fill;
pub mod quad_index;
pub mod service;
pub mod storage;

pub use config::PlateSolveConfig;
pub use service::{solve_frame, solve_frame_with_hints, store_result, SolveResult};

pub use astroimage::platesolving::SolveHints;
pub use storage::{
    delete_plate_solve, get_plate_solve, insert_plate_solve, update_frame_from_solve,
    PlateSolveRecord,
};
