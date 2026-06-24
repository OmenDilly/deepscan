//! Baseline learning v1 — local-distribution anomaly detection.
//!
//! Static signatures catch *known* leaks. This catches *unknown* ones: in a
//! "zone" full of same-class sibling directories (per-bundle temp dirs,
//! caches, app containers), a leak shows up as a wild size outlier. The
//! baseline is *learned from the siblings* (their median), not hand-coded — so
//! a future idleassetsd-style leak is flagged without anyone writing a
//! signature first.
//!
//! v1 learns from the local machine's siblings. A future version can compare
//! against a community median per directory (cross-machine).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::report::Severity;
use crate::scan::measure_path;
use crate::signatures::expand_paths;

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * MB;

/// Flag a child this many times larger than the sibling median (when non-zero).
const RATIO_THRESHOLD: f64 = 10.0;

pub struct Zone {
    pub name: &'static str,
    /// Container whose immediate children are same-class dirs (`~`/`$VARS`/glob ok).
    pub path: &'static str,
    /// Absolute floor (MB) below which a child is never flagged — kills noise.
    pub min_flag_mb: u64,
}

/// Zones where many same-class directories live — the places leaks hide.
pub fn default_zones() -> Vec<Zone> {
    vec![
        Zone {
            name: "Per-user temp",
            path: "$TMPDIR",
            min_flag_mb: 200,
        },
        Zone {
            name: "Per-user temp caches",
            path: "/private/var/folders/*/*/C",
            min_flag_mb: 200,
        },
        Zone {
            name: "User caches",
            path: "~/Library/Caches",
            min_flag_mb: 500,
        },
        Zone {
            name: "App containers",
            path: "~/Library/Containers",
            min_flag_mb: 2000,
        },
        Zone {
            name: "Application Support",
            path: "~/Library/Application Support",
            min_flag_mb: 1000,
        },
        Zone {
            name: "Group containers",
            path: "~/Library/Group Containers",
            min_flag_mb: 500,
        },
    ]
}

#[derive(Debug, Clone, Serialize)]
pub struct Anomaly {
    pub zone: String,
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub median_bytes: u64,
    /// Size relative to the sibling median; `None` when the median is 0
    /// (a lone outlier among otherwise-empty siblings — the idleassetsd shape).
    pub ratio: Option<f64>,
    pub siblings: usize,
    pub severity: Severity,
}

/// Scan every zone and return size outliers, largest first.
pub fn detect_anomalies(zones: &[Zone]) -> Vec<Anomaly> {
    let mut anomalies = Vec::new();
    for zone in zones {
        let floor = zone.min_flag_mb.saturating_mul(MB);
        for container in expand_paths(zone.path) {
            anomalies.extend(analyze_container(zone.name, &container, floor));
        }
    }
    anomalies.sort_by_key(|anomaly| std::cmp::Reverse(anomaly.bytes));
    anomalies
}

/// Flag size outliers among the immediate child directories of `container`.
pub fn analyze_container(zone: &str, container: &Path, floor: u64) -> Vec<Anomaly> {
    let children = sized_children(container);
    if children.len() < 2 {
        return Vec::new(); // need siblings to learn a baseline
    }
    let median = median_bytes(&children);

    let mut anomalies = Vec::new();
    for (path, bytes) in &children {
        let bytes = *bytes;
        if bytes < floor {
            continue;
        }
        let is_outlier = median == 0 || bytes as f64 >= RATIO_THRESHOLD * median as f64;
        if !is_outlier {
            continue;
        }
        let ratio = if median == 0 {
            None
        } else {
            Some(bytes as f64 / median as f64)
        };
        anomalies.push(Anomaly {
            zone: zone.to_string(),
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.clone(),
            bytes,
            median_bytes: median,
            ratio,
            siblings: children.len(),
            severity: severity_for(bytes, ratio),
        });
    }
    anomalies
}

fn sized_children(container: &Path) -> Vec<(PathBuf, u64)> {
    let read_dir = match fs::read_dir(container) {
        Ok(read_dir) => read_dir,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        let is_dir = fs::symlink_metadata(&path)
            .map(|meta| meta.file_type().is_dir())
            .unwrap_or(false);
        if !is_dir {
            continue; // skip files and symlinks
        }
        let bytes = measure_path(&path);
        out.push((path, bytes));
    }
    out
}

fn median_bytes(children: &[(PathBuf, u64)]) -> u64 {
    let mut sizes: Vec<u64> = children.iter().map(|(_, bytes)| *bytes).collect();
    sizes.sort_unstable();
    let n = sizes.len();
    if n == 0 {
        return 0;
    }
    if n % 2 == 1 {
        sizes[n / 2]
    } else {
        (sizes[n / 2 - 1] + sizes[n / 2]) / 2
    }
}

fn severity_for(bytes: u64, ratio: Option<f64>) -> Severity {
    let ratio = ratio.unwrap_or(f64::INFINITY);
    if bytes >= 20 * GB || ratio >= 100.0 {
        Severity::Critical
    } else if bytes >= 2 * GB || ratio >= 25.0 {
        Severity::Warn
    } else {
        Severity::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_lone_outlier_among_empty_siblings() {
        let base = std::env::temp_dir().join(format!("deepscan-anom-{}", std::process::id()));
        // Three tiny siblings + one fat one.
        for name in ["a", "b", "c"] {
            std::fs::create_dir_all(base.join(name)).unwrap();
            std::fs::write(base.join(name).join("x"), vec![0u8; 16]).unwrap();
        }
        std::fs::create_dir_all(base.join("leaker")).unwrap();
        std::fs::write(base.join("leaker").join("big"), vec![0u8; 8 * 1024 * 1024]).unwrap();

        // Floor 1 MB so the 8 MB leaker is flagged but the 16-byte siblings aren't.
        let anomalies = analyze_container("test", &base, 1024 * 1024);
        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].name, "leaker");
        assert_eq!(anomalies[0].siblings, 4);

        let _ = std::fs::remove_dir_all(&base);
    }
}
