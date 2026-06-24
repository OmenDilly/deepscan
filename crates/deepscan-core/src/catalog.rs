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
}

/// The default reclaimable catalog. `~` is expanded at evaluation time.
pub fn default_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry { name: "Xcode DerivedData", category: "dev", path: "~/Library/Developer/Xcode/DerivedData", note: "build artifacts; rebuilds" },
        CatalogEntry { name: "Xcode Archives", category: "dev", path: "~/Library/Developer/Xcode/Archives", note: "keep releases you shipped" },
        CatalogEntry { name: "iOS DeviceSupport", category: "dev", path: "~/Library/Developer/Xcode/iOS DeviceSupport", note: "regenerates on device connect" },
        CatalogEntry { name: "Simulator devices", category: "dev", path: "~/Library/Developer/CoreSimulator/Devices", note: "erase unused: simctl delete unavailable" },
        CatalogEntry { name: "Simulator caches", category: "dev", path: "~/Library/Developer/CoreSimulator/Caches", note: "regenerates" },
        CatalogEntry { name: "User caches", category: "cache", path: "~/Library/Caches", note: "apps rebuild on launch" },
        CatalogEntry { name: "npm cache", category: "cache", path: "~/.npm", note: "re-downloads on install" },
        CatalogEntry { name: "Yarn Berry cache", category: "cache", path: "~/.yarn/berry/cache", note: "re-downloads on install" },
        CatalogEntry { name: "pnpm store", category: "cache", path: "~/Library/pnpm", note: "prune: pnpm store prune" },
        CatalogEntry { name: "CocoaPods", category: "cache", path: "~/.cocoapods", note: "re-fetched on pod install" },
        CatalogEntry { name: "Cargo registry", category: "cache", path: "~/.cargo/registry", note: "re-downloads crates" },
        CatalogEntry { name: "Go modules", category: "cache", path: "~/go/pkg/mod", note: "re-downloads modules" },
        CatalogEntry { name: "Gradle caches", category: "cache", path: "~/.gradle", note: "re-downloads" },
        CatalogEntry { name: "Trash", category: "system", path: "~/.Trash", note: "empty when ready" },
        CatalogEntry { name: "Downloads", category: "user", path: "~/Downloads", note: "your files — review first" },
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
            })
        })
        .collect();

    buckets.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    buckets
}
