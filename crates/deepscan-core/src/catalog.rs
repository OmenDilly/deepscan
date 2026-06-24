//! The broad coverage layer — known reclaimable locations present on most
//! Macs. This is the "parity with best-in-class" floor: always reported with
//! a size, no anomaly logic. Leak detection lives in `signatures`.

use std::path::PathBuf;

use crate::report::Bucket;
use crate::scan::measure_path;

pub struct CatalogEntry {
    pub name: &'static str,
    pub category: &'static str,
    pub path: &'static str,
    pub note: &'static str,
    /// Safe for `reclaim --apply` to delete (regenerable, not user data).
    pub safe_auto: bool,
}

/// The default reclaimable catalog. `~` is expanded at evaluation time.
///
/// `safe_auto` is conservative: only regenerable caches/build output are true.
/// User files (Downloads), things you may need (Archives, simulator devices
/// with app state), and stores with cross-project links (pnpm) are false —
/// reported, but never auto-deleted.
pub fn default_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            name: "Xcode DerivedData",
            category: "dev",
            path: "~/Library/Developer/Xcode/DerivedData",
            note: "build artifacts; rebuilds",
            safe_auto: true,
        },
        CatalogEntry {
            name: "Xcode Archives",
            category: "dev",
            path: "~/Library/Developer/Xcode/Archives",
            note: "keep releases you shipped",
            safe_auto: false,
        },
        CatalogEntry {
            name: "iOS DeviceSupport",
            category: "dev",
            path: "~/Library/Developer/Xcode/iOS DeviceSupport",
            note: "regenerates on device connect",
            safe_auto: true,
        },
        CatalogEntry {
            name: "Simulator devices",
            category: "dev",
            path: "~/Library/Developer/CoreSimulator/Devices",
            note: "erase unused: simctl delete unavailable",
            safe_auto: false,
        },
        CatalogEntry {
            name: "Simulator caches",
            category: "dev",
            path: "~/Library/Developer/CoreSimulator/Caches",
            note: "regenerates",
            safe_auto: true,
        },
        CatalogEntry {
            name: "User caches",
            category: "cache",
            path: "~/Library/Caches",
            note: "apps rebuild on launch",
            safe_auto: true,
        },
        CatalogEntry {
            name: "npm cache",
            category: "cache",
            path: "~/.npm",
            note: "re-downloads on install",
            safe_auto: true,
        },
        CatalogEntry {
            name: "Yarn Berry cache",
            category: "cache",
            path: "~/.yarn/berry/cache",
            note: "re-downloads on install",
            safe_auto: true,
        },
        CatalogEntry {
            name: "pnpm store",
            category: "cache",
            path: "~/Library/pnpm",
            note: "prune instead: pnpm store prune",
            safe_auto: false,
        },
        CatalogEntry {
            name: "CocoaPods",
            category: "cache",
            path: "~/.cocoapods",
            note: "re-fetched on pod install",
            safe_auto: true,
        },
        CatalogEntry {
            name: "Cargo registry",
            category: "cache",
            path: "~/.cargo/registry",
            note: "re-downloads crates",
            safe_auto: true,
        },
        CatalogEntry {
            name: "Go modules",
            category: "cache",
            path: "~/go/pkg/mod",
            note: "re-downloads modules",
            safe_auto: true,
        },
        CatalogEntry {
            name: "Gradle caches",
            category: "cache",
            path: "~/.gradle",
            note: "re-downloads",
            safe_auto: true,
        },
        CatalogEntry {
            name: "Trash",
            category: "system",
            path: "~/.Trash",
            note: "empty via Finder when ready",
            safe_auto: false,
        },
        CatalogEntry {
            name: "Downloads",
            category: "user",
            path: "~/Downloads",
            note: "your files — review first",
            safe_auto: false,
        },
    ]
}

/// Measure each catalog entry that exists, sorted largest first.
pub fn evaluate_catalog(entries: &[CatalogEntry]) -> Vec<Bucket> {
    let mut buckets: Vec<Bucket> = entries
        .iter()
        .filter_map(|entry| {
            let expanded = shellexpand::tilde(entry.path).into_owned();
            let path = PathBuf::from(expanded);
            let bytes = measure_path(&path);
            if bytes == 0 {
                return None;
            }
            Some(Bucket {
                name: entry.name.to_string(),
                category: entry.category.to_string(),
                path,
                bytes,
                note: entry.note.to_string(),
                safe_auto: entry.safe_auto,
            })
        })
        .collect();

    buckets.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    buckets
}
