//! File-level finders that reuse the fast parallel walk: largest (and oldest)
//! files, and exact duplicates. Both feed off [`crate::scan::read_dir_lite`] so
//! they inherit the getattrlistbulk speed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use rayon::prelude::*;
use rayon::Scope;

use crate::report::{DuplicateGroup, LargeFile};
use crate::scan::{read_dir_lite, EntryKind};

const DAY_SECS: u64 = 86_400;
const QUICK_HASH_BYTES: usize = 16 * 1024;

fn days_since(time: Option<SystemTime>, now: SystemTime) -> Option<u64> {
    now.duration_since(time?)
        .ok()
        .map(|d| d.as_secs() / DAY_SECS)
}

// ---------------------------------------------------------------------------
// Largest / oldest files
// ---------------------------------------------------------------------------

/// Files at or above `min_bytes` under `root`, largest first. Only large files
/// are stat'd for times, so the extra work is bounded.
pub fn find_large_files(root: &Path, min_bytes: u64) -> Vec<LargeFile> {
    let out = Mutex::new(Vec::new());
    let now = SystemTime::now();
    rayon::scope(|scope| collect_large(root.to_path_buf(), min_bytes, &out, now, scope));

    let mut files = out.into_inner().unwrap_or_default();
    files.sort_by_key(|file| std::cmp::Reverse(file.bytes));
    files
}

fn collect_large<'scope>(
    dir: PathBuf,
    min_bytes: u64,
    out: &'scope Mutex<Vec<LargeFile>>,
    now: SystemTime,
    scope: &Scope<'scope>,
) {
    for entry in read_dir_lite(&dir) {
        match entry.kind {
            EntryKind::File(size) if size >= min_bytes => {
                let path = dir.join(&entry.name);
                let meta = std::fs::symlink_metadata(&path).ok();
                let modified_days = days_since(meta.as_ref().and_then(|m| m.modified().ok()), now);
                let accessed_days = days_since(meta.as_ref().and_then(|m| m.accessed().ok()), now);
                if let Ok(mut guard) = out.lock() {
                    guard.push(LargeFile {
                        path,
                        bytes: size,
                        modified_days,
                        accessed_days,
                    });
                }
            }
            EntryKind::Dir => {
                let child = dir.join(&entry.name);
                scope.spawn(move |inner| collect_large(child, min_bytes, out, now, inner));
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Exact duplicates
// ---------------------------------------------------------------------------

/// Byte-for-byte duplicate files at or above `min_bytes`, biggest waste first.
///
/// Three cheap-to-expensive stages (the Czkawka method): group by **size**
/// (free, from the walk) → group by a **partial hash** (first 16 KB, rules out
/// near-misses without reading whole files) → confirm with a **full BLAKE3**.
/// Zero false positives.
pub fn find_duplicates(root: &Path, min_bytes: u64) -> Vec<DuplicateGroup> {
    let collected = Mutex::new(Vec::<(PathBuf, u64)>::new());
    rayon::scope(|scope| collect_sized(root.to_path_buf(), min_bytes, &collected, scope));
    let files = collected.into_inner().unwrap_or_default();

    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for (path, size) in files {
        by_size.entry(size).or_default().push(path);
    }
    let candidates: Vec<(u64, Vec<PathBuf>)> =
        by_size.into_iter().filter(|(_, v)| v.len() >= 2).collect();

    let mut groups: Vec<DuplicateGroup> = candidates
        .into_par_iter()
        .flat_map(|(size, paths)| {
            confirm_duplicates(paths)
                .into_iter()
                .map(move |paths| DuplicateGroup {
                    bytes: size,
                    wasted: size * (paths.len() as u64 - 1),
                    paths,
                })
                .collect::<Vec<_>>()
        })
        .collect();

    groups.sort_by_key(|group| std::cmp::Reverse(group.wasted));
    groups
}

fn collect_sized<'scope>(
    dir: PathBuf,
    min_bytes: u64,
    out: &'scope Mutex<Vec<(PathBuf, u64)>>,
    scope: &Scope<'scope>,
) {
    for entry in read_dir_lite(&dir) {
        match entry.kind {
            EntryKind::File(size) if size >= min_bytes => {
                if let Ok(mut guard) = out.lock() {
                    guard.push((dir.join(&entry.name), size));
                }
            }
            EntryKind::Dir => {
                let child = dir.join(&entry.name);
                scope.spawn(move |inner| collect_sized(child, min_bytes, out, inner));
            }
            _ => {}
        }
    }
}

/// Within a same-size group, return only the genuinely-identical subsets.
fn confirm_duplicates(paths: Vec<PathBuf>) -> Vec<Vec<PathBuf>> {
    let mut by_quick: HashMap<[u8; 32], Vec<PathBuf>> = HashMap::new();
    for path in paths {
        if let Some(hash) = quick_hash(&path) {
            by_quick.entry(hash).or_default().push(path);
        }
    }

    let mut confirmed = Vec::new();
    for (_, group) in by_quick {
        if group.len() < 2 {
            continue;
        }
        let mut by_full: HashMap<[u8; 32], Vec<PathBuf>> = HashMap::new();
        for path in group {
            if let Some(hash) = full_hash(&path) {
                by_full.entry(hash).or_default().push(path);
            }
        }
        for (_, identical) in by_full {
            if identical.len() >= 2 {
                confirmed.push(identical);
            }
        }
    }
    confirmed
}

fn quick_hash(path: &Path) -> Option<[u8; 32]> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; QUICK_HASH_BYTES];
    let read = file.read(&mut buf).ok()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&buf[..read]);
    Some(*hasher.finalize().as_bytes())
}

fn full_hash(path: &Path) -> Option<[u8; 32]> {
    let file = std::fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file).ok()?;
    Some(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_exact_duplicates_only() {
        let base = std::env::temp_dir().join(format!("deepscan-dupes-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let dup = vec![7u8; 2 * 1024 * 1024];
        std::fs::write(base.join("a.bin"), &dup).unwrap();
        std::fs::write(base.join("b.bin"), &dup).unwrap();
        // same size, different content → must NOT be grouped.
        let mut other = dup.clone();
        other[0] = 0;
        std::fs::write(base.join("c.bin"), &other).unwrap();

        let groups = find_duplicates(&base, 1024 * 1024);
        assert_eq!(groups.len(), 1, "exactly one duplicate set");
        assert_eq!(groups[0].paths.len(), 2);
        assert_eq!(groups[0].wasted, 2 * 1024 * 1024);

        let _ = std::fs::remove_dir_all(&base);
    }
}
