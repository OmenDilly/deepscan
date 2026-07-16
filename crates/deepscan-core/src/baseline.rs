//! Temporal self-baseline — the local half of real anomaly detection. Each run
//! snapshots the sizes of the anomaly-zone folders, compares to the most recent
//! prior snapshot, and reports what grew abnormally fast (or appeared and
//! ballooned). History lives in `~/.deepscan/history.json` (rolling, last 30).
//! This turns "big" into "abnormal vs its own past".

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::anomaly::{classify, default_zones, sized_children, AnomalyKind};
use crate::engine::home_dir;
use crate::signatures::expand_paths;

const MB: u64 = 1024 * 1024;
/// Minimum absolute growth to flag.
const GROWTH_FLOOR: u64 = 500 * MB;
/// Minimum relative growth to flag (25%).
const GROWTH_PCT: f64 = 0.25;
/// Rolling history cap.
const MAX_HISTORY: usize = 30;

/// A point-in-time record of the anomaly-zone folder sizes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unix seconds when taken.
    pub taken: u64,
    /// Folder path (as string) → size in bytes.
    pub sizes: BTreeMap<String, u64>,
}

/// One folder that grew notably (or newly appeared large) since the prior snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct Growth {
    pub path: PathBuf,
    pub name: String,
    pub kind: AnomalyKind,
    pub current: u64,
    pub previous: Option<u64>,
    /// Absolute growth (or the full size, if new).
    pub delta: u64,
    /// Percent growth vs the prior size (None if new).
    pub pct: Option<f64>,
    /// Days between the prior snapshot and this one.
    pub days: f64,
    pub is_new: bool,
}

/// Result of a `growth` run.
#[derive(Debug, Clone, Serialize)]
pub struct GrowthReport {
    /// Timestamp of the prior snapshot compared against (None on the first run).
    pub previous_at: Option<u64>,
    pub now: u64,
    pub entries: Vec<Growth>,
    /// True when there was no prior snapshot — a baseline was recorded only.
    pub baseline_only: bool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Snapshot the size of every child folder across the anomaly zones.
pub fn snapshot_now() -> Snapshot {
    let mut sizes = BTreeMap::new();
    for zone in default_zones() {
        for container in expand_paths(zone.path) {
            for (path, bytes) in sized_children(&container) {
                sizes.insert(path.to_string_lossy().into_owned(), bytes);
            }
        }
    }
    Snapshot {
        taken: now_secs(),
        sizes,
    }
}

fn history_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".deepscan").join("history.json"))
}

/// Load the rolling snapshot history; empty on missing/corrupt.
pub fn load_history() -> Vec<Snapshot> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Persist the history (best-effort; creates `~/.deepscan/`).
pub fn save_history(history: &[Snapshot]) {
    let Some(path) = history_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(history) {
        let _ = std::fs::write(&path, text);
    }
}

/// Compare `current` against `prev`, returning growth entries ranked by delta.
/// Flags a grown folder when it added ≥ 500 MB AND ≥ 25%; a new folder when it
/// is ≥ 500 MB. Pure — no IO.
pub fn compute_growth(prev: &Snapshot, current: &Snapshot) -> Vec<Growth> {
    let days = (current.taken.saturating_sub(prev.taken) as f64) / 86_400.0;
    let mut out = Vec::new();
    for (path_str, &cur) in &current.sizes {
        let previous = prev.sizes.get(path_str).copied();
        let (delta, pct, is_new) = match previous {
            Some(p) => {
                if cur <= p {
                    continue;
                }
                let delta = cur - p;
                let ratio = if p > 0 {
                    delta as f64 / p as f64
                } else {
                    f64::INFINITY
                };
                if delta < GROWTH_FLOOR || ratio < GROWTH_PCT {
                    continue;
                }
                (delta, Some(ratio * 100.0), false)
            }
            None => {
                if cur < GROWTH_FLOOR {
                    continue;
                }
                (cur, None, true)
            }
        };
        let path = PathBuf::from(path_str);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path_str.clone());
        out.push(Growth {
            kind: classify(&path),
            path,
            name,
            current: cur,
            previous,
            delta,
            pct,
            days,
            is_new,
        });
    }
    out.sort_by_key(|g| std::cmp::Reverse(g.delta));
    out
}

/// Take a snapshot, compare to the most recent prior one, append + prune + save.
pub fn record_and_compare() -> GrowthReport {
    let mut history = load_history();
    let current = snapshot_now();
    let now = current.taken;

    let report = match history.last() {
        Some(prev) => GrowthReport {
            previous_at: Some(prev.taken),
            now,
            entries: compute_growth(prev, &current),
            baseline_only: false,
        },
        None => GrowthReport {
            previous_at: None,
            now,
            entries: Vec::new(),
            baseline_only: true,
        },
    };

    history.push(current);
    let excess = history.len().saturating_sub(MAX_HISTORY);
    if excess > 0 {
        history.drain(0..excess);
    }
    save_history(&history);

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(taken: u64, entries: &[(&str, u64)]) -> Snapshot {
        Snapshot {
            taken,
            sizes: entries.iter().map(|(p, b)| (p.to_string(), *b)).collect(),
        }
    }

    const GB: u64 = 1024 * MB;

    #[test]
    fn flags_big_fast_growth_not_small_ticks() {
        let day = 86_400;
        let prev = snap(
            0,
            &[
                ("/Users/x/Library/Application Support/Claude", 2 * GB),
                ("/Users/x/Library/Caches/steady", 10 * GB),
            ],
        );
        let current = snap(
            3 * day,
            &[
                ("/Users/x/Library/Application Support/Claude", 20 * GB), // +18 GB, +900%
                ("/Users/x/Library/Caches/steady", 10 * GB + 100 * MB), // +100 MB, +1% → below floor+pct
            ],
        );
        let g = compute_growth(&prev, &current);
        assert_eq!(g.len(), 1, "only the big + fast grower is flagged");
        assert_eq!(g[0].name, "Claude");
        assert_eq!(g[0].kind, AnomalyKind::AppData);
        assert!(!g[0].is_new);
        assert!((g[0].days - 3.0).abs() < 0.001);
    }

    #[test]
    fn flags_new_large_folder() {
        let prev = snap(0, &[("/Users/x/Library/Caches/old", MB)]);
        let current = snap(
            86_400,
            &[
                ("/Users/x/Library/Caches/old", MB),
                ("/Users/x/Library/Application Support/Leak", 3 * GB), // new, 3 GB
            ],
        );
        let g = compute_growth(&prev, &current);
        assert_eq!(g.len(), 1);
        assert!(g[0].is_new);
        assert_eq!(g[0].name, "Leak");
        assert_eq!(g[0].previous, None);
    }

    #[test]
    fn small_new_folder_not_flagged() {
        let prev = snap(0, &[]);
        let current = snap(86_400, &[("/Users/x/Library/Caches/tiny", 10 * MB)]);
        assert!(compute_growth(&prev, &current).is_empty());
    }

    #[test]
    fn snapshot_json_round_trips() {
        // Round-trip through serde against a TEMP file — never touch ~/.deepscan.
        let base = std::env::temp_dir().join(format!("deepscan-hist-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("history.json");
        let history = vec![snap(1, &[("/a", 10)]), snap(2, &[("/a", 20)])];
        std::fs::write(&file, serde_json::to_string(&history).unwrap()).unwrap();
        let loaded: Vec<Snapshot> =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].sizes.get("/a"), Some(&20));
        let _ = std::fs::remove_dir_all(&base);
    }
}
