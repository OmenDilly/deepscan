//! Biggest same-class folders, honestly classified.
//!
//! In a "zone" full of same-class sibling directories (caches, per-bundle temp
//! dirs, app containers), the heavy children stand out. We surface them — but
//! the honest signal is **what kind of folder it is**, not "N× the sibling
//! median". That multiple is degenerate on the heavy-tailed size distributions
//! these zones always have (a handful of GB-scale apps among hundreds of KB-
//! scale config dirs), so it reads as astronomical for *every* large folder on
//! *every* Mac and means nothing.
//!
//! Instead we cross-reference each folder against the reclaimable catalog and
//! the known cache/temp locations:
//!   • [`AnomalyKind::Cache`]   — regenerable, safe to clear. Expected to be
//!     big; never alarming. Severity is always `Info`.
//!   • [`AnomalyKind::AppData`] — real app state (Application Support /
//!     Containers / Group Containers). A large unexplained one is the thing
//!     actually worth a look, so severity scales with size.
//!
//! A large idleassetsd-style leak still surfaces — as a big *AppData* folder
//! that isn't a known cache — which is exactly the case worth investigating.
//! The sibling median is kept in the payload for reference, not as the headline.
//!
//! A future version can compare against a community median per class
//! (cross-machine) — the only comparison that turns "big" into "abnormal".

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::catalog::default_catalog;
use crate::report::Severity;
use crate::scan::measure_path;
use crate::signatures::expand_paths;

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * MB;

/// Flag a child this many times larger than the sibling median (when non-zero).
const RATIO_THRESHOLD: f64 = 10.0;

/// What kind of heavy folder this is — the honest classification that replaces
/// the meaningless median multiple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnomalyKind {
    /// A cache / per-user temp dir: regenerable, safe to clear. Expected to be
    /// large — informational, never a problem.
    Cache,
    /// App state (Application Support / Containers / Group Containers): real
    /// data, not auto-clearable. A big one is worth checking before removing.
    AppData,
}

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
    /// Cache (safe to clear) vs app data (review) — the honest headline.
    pub kind: AnomalyKind,
    pub median_bytes: u64,
    /// Size relative to the sibling median. Kept for reference only — it is
    /// degenerate on these heavy-tailed distributions and is no longer the
    /// basis for severity. `None` when the median is 0 (a lone outlier).
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

/// Every child folder across all zones, with its zone name — the full
/// population. This is what a *typical* size must be sampled from;
/// [`detect_anomalies`] deliberately returns only the outlier subset.
pub fn zone_children() -> Vec<(&'static str, PathBuf, u64)> {
    let mut out = Vec::new();
    for zone in default_zones() {
        for container in expand_paths(zone.path) {
            for (path, bytes) in sized_children(&container) {
                out.push((zone.name, path, bytes));
            }
        }
    }
    out
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
        let kind = classify(path);
        anomalies.push(Anomaly {
            zone: zone.to_string(),
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.clone(),
            bytes,
            kind,
            median_bytes: median,
            ratio,
            siblings: children.len(),
            severity: severity_for(kind, bytes),
        });
    }
    anomalies
}

/// Cache (regenerable, safe to clear) vs real app data, decided from the path:
/// a known cache/temp location, or a `safe_auto` cache entry in the reclaimable
/// catalog, is a cache; everything else is app data worth reviewing.
pub(crate) fn classify(path: &Path) -> AnomalyKind {
    if is_reclaimable_cache(path) {
        AnomalyKind::Cache
    } else {
        AnomalyKind::AppData
    }
}

fn is_reclaimable_cache(path: &Path) -> bool {
    // Structural: `~/Library/Caches`, any `Caches` ancestor, and everything
    // under the per-user temp scratch (`/var/folders/**`, both the `T` and `C`
    // dirs) is regenerable by definition.
    let text = path.to_string_lossy();
    if text.contains("/Library/Caches/")
        || text.contains("/var/folders/")
        || path
            .ancestors()
            .any(|a| a.file_name().and_then(|n| n.to_str()) == Some("Caches"))
    {
        return true;
    }
    // Catalog cross-reference: inside a known safe-to-clear cache location.
    default_catalog().iter().any(|entry| {
        entry.safe_auto && entry.category == "cache" && {
            let root = PathBuf::from(shellexpand::tilde(entry.path).into_owned());
            path == root || path.starts_with(&root)
        }
    })
}

pub(crate) fn sized_children(container: &Path) -> Vec<(PathBuf, u64)> {
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

/// Severity from what the folder *is*, not from the (degenerate) median ratio.
/// A cache is safe and expected — never more than informational. App data is
/// the real signal, so a big unexplained one escalates with size.
fn severity_for(kind: AnomalyKind, bytes: u64) -> Severity {
    match kind {
        AnomalyKind::Cache => Severity::Info,
        AnomalyKind::AppData if bytes >= 10 * GB => Severity::Critical,
        AnomalyKind::AppData if bytes >= 2 * GB => Severity::Warn,
        AnomalyKind::AppData => Severity::Info,
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

    #[test]
    fn classifies_caches_vs_app_data() {
        // Caches, any `Caches` ancestor, and per-user temp scratch are caches.
        assert_eq!(
            classify(Path::new("/Users/x/Library/Caches/com.spotify.client")),
            AnomalyKind::Cache
        );
        assert_eq!(
            classify(Path::new("/private/var/folders/aa/bb/C/com.foo")),
            AnomalyKind::Cache
        );
        // Application Support / Containers hold real state → app data.
        assert_eq!(
            classify(Path::new("/Users/x/Library/Application Support/Claude")),
            AnomalyKind::AppData
        );
        assert_eq!(
            classify(Path::new("/Users/x/Library/Group Containers/2BBY.dev.warp")),
            AnomalyKind::AppData
        );
    }

    #[test]
    fn severity_tracks_kind_not_ratio() {
        // A huge cache is still only informational — it's safe and expected.
        assert_eq!(severity_for(AnomalyKind::Cache, 20 * GB), Severity::Info);
        // App data escalates with size: the real signal.
        assert_eq!(
            severity_for(AnomalyKind::AppData, 20 * GB),
            Severity::Critical
        );
        assert_eq!(severity_for(AnomalyKind::AppData, 3 * GB), Severity::Warn);
        assert_eq!(severity_for(AnomalyKind::AppData, 500 * MB), Severity::Info);
    }
}
