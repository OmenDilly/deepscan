//! deepscan-core — the engine. One fast parallel scanner, a broad reclaimable
//! catalog, and a data-driven leak-signature evaluator.
//!
//! The CLI links this today; a future Swift menu-bar app can link the same
//! crate built as a `cdylib` behind a C ABI — write the fast, root-adjacent
//! path once.

pub mod catalog;
pub mod report;
pub mod scan;
pub mod signatures;

pub use catalog::{default_catalog, evaluate_catalog, CatalogEntry};
pub use report::{human, Bucket, ChildSize, Finding, Severity};
pub use scan::{count_matching_files, measure_path, scan_children};
pub use signatures::{
    default_signatures, evaluate_signatures, load_signatures, parse_signatures, Signature,
};
