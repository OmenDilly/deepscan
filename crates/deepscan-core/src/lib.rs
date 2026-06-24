//! deepscan-core — the engine. One fast parallel scanner, a broad reclaimable
//! catalog, a data-driven leak-signature evaluator, and a guarded reclaim.
//!
//! The CLI links this today; a future Swift menu-bar app can link the same
//! crate built as a `cdylib` behind a C ABI — write the fast, root-adjacent
//! path once.

pub mod anomaly;
pub mod catalog;
pub mod engine;
pub mod report;
pub mod scan;
pub mod signatures;

pub use anomaly::{analyze_container, default_zones, detect_anomalies, Anomaly, Zone};
pub use catalog::{default_catalog, evaluate_catalog, CatalogEntry};
pub use engine::{
    build_reclaim_plan, build_report, execute_reclaim, home_dir, is_safe_to_delete, ReclaimOutcome,
    ReclaimPlan, ReclaimResult, ReclaimTarget,
};
pub use report::{human, Bucket, ChildSize, Finding, ScanReport, Severity, TreeNode};
pub use scan::{build_tree, count_matching_files, measure_path, scan_children};
pub use signatures::{
    default_signatures, evaluate_signatures, load_signatures, parse_signatures, Signature,
};
