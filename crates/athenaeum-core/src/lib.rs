// Athenaeum Core Library
// Shared business logic for desktop (Tauri) and web (Axum) frontends

pub mod models;
pub mod coordinates;
pub mod fingerprint;
pub mod calibration;
pub mod db;
pub mod fits_parser;
pub mod clustering;
pub mod settings;
pub mod sessions;
pub mod duplicates;
pub mod relinking;
pub mod frames_set_metadata;
pub mod frames_set_merge;
pub mod logging;
pub mod events;
pub mod scanner;
pub mod monitor;
pub mod auto_merge;
pub mod export;
pub mod rustafits_processor;
pub mod cache;
pub mod analysis;
pub mod catalog;
pub mod plate_solve;
pub mod services;
