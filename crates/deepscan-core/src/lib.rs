//! deepscan-core — the engine. One fast parallel scanner, a broad reclaimable
//! catalog, a data-driven leak-signature evaluator, and a guarded reclaim.
//!
//! The CLI links this today; a future Swift menu-bar app can link the same
//! crate built as a `cdylib` behind a C ABI — write the fast, root-adjacent
//! path once.

pub mod anomaly;
pub mod baseline;
pub mod catalog;
pub mod deletion;
pub mod engine;
pub mod files;
pub mod report;
pub mod scan;
pub mod signatures;
pub mod space;
pub mod uninstall;

pub use anomaly::{analyze_container, default_zones, detect_anomalies, Anomaly, AnomalyKind, Zone};
pub use baseline::{
    compute_growth, load_history, record_and_compare, save_history, snapshot_now, Growth,
    GrowthReport, Snapshot,
};
pub use catalog::{default_catalog, evaluate_catalog, CatalogEntry};
pub use deletion::move_to_trash;
pub use engine::{
    build_reclaim_plan, build_report, execute_reclaim, execute_reclaim_with, home_dir,
    is_safe_to_delete, is_safe_to_trash, ReclaimOutcome, ReclaimPlan, ReclaimResult, ReclaimTarget,
};
pub use files::{find_duplicates, find_large_files};
pub use report::{
    human, Bucket, ChildSize, Confidence, DiskSpace, DuplicateGroup, Finding, LargeFile, Leftover,
    ScanReport, Severity, SpaceReport, TreeNode, UninstallOutcome, UninstallPlan,
};
pub use scan::{
    build_tree, count_matching_files, measure_path, measure_path_serial, progress, reset_progress,
    scan_children,
};
pub use signatures::{
    default_signatures, evaluate_signatures, load_signatures, parse_signatures, Signature,
};
pub use space::{disk_space, local_snapshots, space_report};
pub use uninstall::{execute_uninstall, plan_uninstall};
