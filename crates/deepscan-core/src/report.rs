//! Shared result types and formatting helpers. All are `Serialize` so the CLI
//! can emit `--json` straight from the engine output.

use std::path::PathBuf;

use serde::Serialize;

/// Size of one immediate child of a scanned root (the "where did it go" view).
#[derive(Debug, Clone, Serialize)]
pub struct ChildSize {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
}

/// A node in a depth-bounded size tree (the `--depth` view). Leaf directories
/// at the depth frontier carry their full recursive size with no children.
#[derive(Debug, Clone, Serialize)]
pub struct TreeNode {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub is_dir: bool,
    pub children: Vec<TreeNode>,
}

/// A known reclaimable location (the broad, every-Mac coverage).
#[derive(Debug, Clone, Serialize)]
pub struct Bucket {
    pub name: String,
    pub category: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub note: String,
    /// Safe for `reclaim --apply` to delete automatically (regenerable, not
    /// user data). User files (Downloads) and things you might need
    /// (Archives, simulator devices) are `false` — shown but never auto-deleted.
    pub safe_auto: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

/// A leak/anomaly flagged by a signature whose path exceeded its baseline.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub baseline_bytes: u64,
    pub owner: Option<String>,
    pub severity: Severity,
    pub root_cause: Option<String>,
    pub prevention: Option<String>,
    pub safe_delete: String,
    pub file_matches: Option<u64>,
}

/// The full result of a `scan` — the JSON payload, and the source the human
/// renderer reads from.
#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub root: PathBuf,
    pub total_bytes: u64,
    pub children: Vec<ChildSize>,
    /// Present only when `--depth >= 2`; the nested size tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeNode>,
    pub reclaimable_bytes: u64,
    pub buckets: Vec<Bucket>,
    pub findings: Vec<Finding>,
}

/// Human-readable byte size, e.g. `161.4 GB`.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}
