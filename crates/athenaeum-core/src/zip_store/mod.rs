//! Zip I/O for the catalog: no other module opens a zip.
//!
//! `reader` reads a zip's directory and the first bytes of its entries (the metadata scan) and
//! extracts one entry to a local file. `cache` keeps extracted entries in a size-limited directory and
//! `materialize` turns catalog paths into local paths through it. Zips are read-only here.

pub mod cache;
pub mod materialize;
pub mod presence;
pub mod reader;
pub mod scan;
