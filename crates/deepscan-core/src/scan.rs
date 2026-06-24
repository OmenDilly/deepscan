//! The fast path: parallel directory sizing.
//!
//! v0 uses `rayon::scope` + `spawn`: one task per *directory*, with files
//! summed inline into an atomic. `spawn` never blocks the spawning worker, so
//! unlike a recursive `par_iter().sum()` this cannot starve the thread pool.
//! Stats run across many workers in parallel; the outer `scope` returns once
//! every descendant task has completed.
//!
//! Symlinks are never followed, so volumes/firmlinks are not double counted
//! and there are no traversal cycles. Unreadable entries count as 0.
//!
//! TODO(perf): replace the per-file `symlink_metadata` stat with a single
//! `getattrlistbulk(2)` per directory — one syscall returns sizes for a whole
//! directory instead of one stat per file. That is the real "instant" unlock.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glob::Pattern;
use rayon::Scope;

use crate::report::ChildSize;

/// Total apparent size (sum of file lengths) of everything under `path`.
pub fn measure_path(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => return 0,
    };
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return 0;
    }
    if file_type.is_file() {
        return meta.len();
    }
    if !file_type.is_dir() {
        return 0;
    }

    let total = AtomicU64::new(0);
    rayon::scope(|scope| sum_dir(path.to_path_buf(), &total, scope));
    total.load(Ordering::Relaxed)
}

fn sum_dir<'scope>(dir: PathBuf, total: &'scope AtomicU64, scope: &Scope<'scope>) {
    let read_dir = match fs::read_dir(&dir) {
        Ok(read_dir) => read_dir,
        Err(_) => return,
    };

    let mut local = 0u64;
    for entry in read_dir.filter_map(Result::ok) {
        let child = entry.path();
        let meta = match fs::symlink_metadata(&child) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_file() {
            local += meta.len();
        } else if file_type.is_dir() {
            scope.spawn(move |inner| sum_dir(child, total, inner));
        }
    }
    if local > 0 {
        total.fetch_add(local, Ordering::Relaxed);
    }
}

/// Count files under `dir` whose file name matches `pattern` (e.g. the
/// `CFNetworkDownload_*.tmp` fragments of the idleassetsd leak).
pub fn count_matching_files(dir: &Path, pattern: &Pattern) -> u64 {
    let count = AtomicU64::new(0);
    rayon::scope(|scope| count_dir(dir.to_path_buf(), pattern, &count, scope));
    count.load(Ordering::Relaxed)
}

fn count_dir<'scope>(
    dir: PathBuf,
    pattern: &'scope Pattern,
    count: &'scope AtomicU64,
    scope: &Scope<'scope>,
) {
    let read_dir = match fs::read_dir(&dir) {
        Ok(read_dir) => read_dir,
        Err(_) => return,
    };

    let mut local = 0u64;
    for entry in read_dir.filter_map(Result::ok) {
        let child = entry.path();
        let meta = match fs::symlink_metadata(&child) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_file() {
            let matched = child
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| pattern.matches(name))
                .unwrap_or(false);
            if matched {
                local += 1;
            }
        } else if file_type.is_dir() {
            scope.spawn(move |inner| count_dir(child, pattern, count, inner));
        }
    }
    if local > 0 {
        count.fetch_add(local, Ordering::Relaxed);
    }
}

/// Size every immediate child of `root`, returning `(total, children desc)`.
///
/// Children are scanned sequentially; each `measure_path` is itself fully
/// parallel, so the largest subtree still uses every core.
pub fn scan_children(root: &Path) -> io::Result<(u64, Vec<ChildSize>)> {
    let entries: Vec<_> = fs::read_dir(root)?.filter_map(Result::ok).collect();

    let mut children: Vec<ChildSize> = entries
        .iter()
        .map(|entry| {
            let path = entry.path();
            let bytes = measure_path(&path);
            ChildSize {
                name: entry.file_name().to_string_lossy().into_owned(),
                path,
                bytes,
            }
        })
        .collect();

    children.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    let total = children.iter().map(|child| child.bytes).sum();
    Ok((total, children))
}
