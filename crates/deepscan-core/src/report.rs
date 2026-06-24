//! Shared result types and formatting helpers.

use std::path::PathBuf;

/// Size of one immediate child of a scanned root (the "where did it go" view).
#[derive(Debug, Clone)]
pub struct ChildSize {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
}

/// A known reclaimable location (the broad, every-Mac coverage).
#[derive(Debug, Clone)]
pub struct Bucket {
    pub name: String,
    pub category: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

/// A leak/anomaly flagged by a signature whose path exceeded its baseline.
#[derive(Debug, Clone)]
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
