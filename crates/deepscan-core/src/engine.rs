//! Orchestration on top of the scanner: assemble a [`ScanReport`], and plan /
//! execute a guarded reclaim.

use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::catalog::{default_catalog, evaluate_catalog};
use crate::report::ScanReport;
use crate::scan::scan_children;
use crate::signatures::{evaluate_signatures, Signature};

/// Build the full scan report: tree breakdown, reclaimable buckets, and fired
/// leak signatures. `top` truncates the child list; `include_tree` skips the
/// (independent) child breakdown when false.
pub fn build_report(
    root: &Path,
    top: usize,
    include_tree: bool,
    signatures: &[Signature],
) -> io::Result<ScanReport> {
    let (total_bytes, mut children) = if include_tree {
        scan_children(root)?
    } else {
        (0, Vec::new())
    };
    children.retain(|child| child.bytes > 0);
    children.truncate(top);

    let buckets = evaluate_catalog(&default_catalog());
    let reclaimable_bytes = buckets.iter().map(|bucket| bucket.bytes).sum();
    let findings = evaluate_signatures(signatures);

    Ok(ScanReport {
        root: root.to_path_buf(),
        total_bytes,
        children,
        reclaimable_bytes,
        buckets,
        findings,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ReclaimTarget {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub note: String,
    /// True when this target is safe to delete automatically.
    pub auto: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReclaimPlan {
    /// Regenerable, safe to delete with `--apply`.
    pub auto_targets: Vec<ReclaimTarget>,
    /// Reported, but you must review/run these yourself (user files, system
    /// paths needing sudo, stores with cross-project links).
    pub manual_targets: Vec<ReclaimTarget>,
    pub auto_bytes: u64,
}

/// Partition the catalog into auto-safe vs manual reclaim targets.
pub fn build_reclaim_plan() -> ReclaimPlan {
    let home = home_dir();
    let mut auto_targets = Vec::new();
    let mut manual_targets = Vec::new();

    for bucket in evaluate_catalog(&default_catalog()) {
        let auto = bucket.safe_auto && is_safe_to_delete(&bucket.path, home.as_deref());
        let target = ReclaimTarget {
            name: bucket.name,
            path: bucket.path,
            bytes: bucket.bytes,
            note: bucket.note,
            auto,
        };
        if auto {
            auto_targets.push(target);
        } else {
            manual_targets.push(target);
        }
    }

    let auto_bytes = auto_targets.iter().map(|target| target.bytes).sum();
    ReclaimPlan {
        auto_targets,
        manual_targets,
        auto_bytes,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReclaimOutcome {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReclaimResult {
    pub deleted: Vec<ReclaimOutcome>,
    pub freed_bytes: u64,
}

/// Delete each target (recursively), refusing any path that fails the safety
/// guard. `home` is injected so this is testable against a temp fixture.
pub fn execute_reclaim(targets: &[ReclaimTarget], home: Option<&Path>) -> ReclaimResult {
    let mut deleted = Vec::new();
    let mut freed_bytes = 0u64;

    for target in targets {
        if !is_safe_to_delete(&target.path, home) {
            deleted.push(ReclaimOutcome {
                name: target.name.clone(),
                path: target.path.clone(),
                bytes: target.bytes,
                ok: false,
                error: Some("refused: failed safety guard".to_string()),
            });
            continue;
        }
        match std::fs::remove_dir_all(&target.path) {
            Ok(()) => {
                freed_bytes += target.bytes;
                deleted.push(ReclaimOutcome {
                    name: target.name.clone(),
                    path: target.path.clone(),
                    bytes: target.bytes,
                    ok: true,
                    error: None,
                });
            }
            Err(err) => deleted.push(ReclaimOutcome {
                name: target.name.clone(),
                path: target.path.clone(),
                bytes: target.bytes,
                ok: false,
                error: Some(err.to_string()),
            }),
        }
    }

    ReclaimResult {
        deleted,
        freed_bytes,
    }
}

/// Refuse to delete: relative paths, anything with `..`, the filesystem root or
/// near-root, and the home directory itself. Catalog entries live at
/// `$HOME/<dir>/...` (4+ components), so real targets pass while dangerous
/// paths are rejected.
pub fn is_safe_to_delete(path: &Path, home: Option<&Path>) -> bool {
    if !path.is_absolute() {
        return false;
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    // RootDir + at least 3 named components, e.g. /Users/<user>/<dir>.
    if path.components().count() < 4 {
        return false;
    }
    if let Some(home) = home {
        if path == home {
            return false;
        }
    }
    true
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_rejects_dangerous_paths() {
        let home = PathBuf::from("/Users/tester");
        assert!(!is_safe_to_delete(Path::new("/"), Some(&home)));
        assert!(!is_safe_to_delete(Path::new("/Users"), Some(&home)));
        assert!(!is_safe_to_delete(Path::new("/Users/tester"), Some(&home)));
        assert!(!is_safe_to_delete(Path::new("relative/path"), Some(&home)));
        assert!(!is_safe_to_delete(
            Path::new("/Users/tester/../etc"),
            Some(&home)
        ));
        assert!(is_safe_to_delete(
            Path::new("/Users/tester/Library/Caches"),
            Some(&home)
        ));
    }

    #[test]
    fn execute_reclaim_deletes_only_safe_targets() {
        // Build an isolated fixture under the temp dir.
        let base = std::env::temp_dir().join(format!("deepscan-test-{}", std::process::id()));
        let victim = base.join("cache_dir");
        std::fs::create_dir_all(victim.join("nested")).unwrap();
        std::fs::write(victim.join("nested").join("a.bin"), vec![0u8; 1024]).unwrap();

        let targets = vec![
            ReclaimTarget {
                name: "fixture cache".to_string(),
                path: victim.clone(),
                bytes: 1024,
                note: String::new(),
                auto: true,
            },
            // A path that fails the guard must be refused, not deleted.
            ReclaimTarget {
                name: "root".to_string(),
                path: PathBuf::from("/"),
                bytes: 0,
                note: String::new(),
                auto: true,
            },
        ];

        // home far away so the fixture passes the guard but is still isolated.
        let result = execute_reclaim(&targets, Some(Path::new("/nonexistent-home")));

        assert_eq!(result.freed_bytes, 1024);
        assert!(!victim.exists(), "safe target should be deleted");
        assert!(result.deleted[0].ok);
        assert!(!result.deleted[1].ok, "root must be refused");

        let _ = std::fs::remove_dir_all(&base);
    }
}
