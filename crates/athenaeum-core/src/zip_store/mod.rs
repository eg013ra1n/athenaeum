//! Zip I/O for the catalog: no other module opens a zip.
//!
//! `reader` reads a zip's directory and the first bytes of its entries (the metadata scan) and
//! extracts one entry to a local file. Zips are read-only here.

pub mod reader;
