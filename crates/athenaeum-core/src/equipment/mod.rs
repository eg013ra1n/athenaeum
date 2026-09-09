//! Catalog-only optical configurations and reviewed scale matches.
//! Geometry is pure; storage and API orchestration never write image headers.
pub mod matching;
pub mod models;
pub mod storage;
#[cfg(test)]
mod tests;
