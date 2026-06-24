//! The fast path: parallel directory sizing.
//!
//! Orchestration is `rayon::scope` + `spawn`: one task per *directory*, files
//! summed inline into an atomic. `spawn` never blocks the spawning worker, so
//! unlike a recursive `par_iter().sum()` this cannot starve the thread pool.
//!
//! Each directory is listed through [`read_dir_lite`], which has two backends:
//!   * **macOS** — `getattrlistbulk(2)`: one syscall returns the name, type,
//!     and size of *every* child at once. This collapses the N per-file
//!     `stat()` calls (which dominated wall time — it was ~95% sys time) into
//!     a handful of syscalls per directory.
//!   * **other** — a portable `read_dir` + `symlink_metadata` fallback so the
//!     crate still builds and runs off macOS (CI, Linux dev).
//!
//! Symlinks are never followed: volumes/firmlinks are not double counted and
//! there are no traversal cycles. Unreadable entries count as 0.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glob::Pattern;
use rayon::Scope;

use crate::report::ChildSize;

/// A lightweight directory entry — just what the sizer needs.
enum EntryKind {
    File(u64),
    Dir,
    Skip,
}

struct LiteEntry {
    name: OsString,
    kind: EntryKind,
}

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
    let mut local = 0u64;
    for entry in read_dir_lite(&dir) {
        match entry.kind {
            EntryKind::File(size) => local += size,
            EntryKind::Dir => {
                let child = dir.join(&entry.name);
                scope.spawn(move |inner| sum_dir(child, total, inner));
            }
            EntryKind::Skip => {}
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
    let mut local = 0u64;
    for entry in read_dir_lite(&dir) {
        match entry.kind {
            EntryKind::File(_) => {
                let matched = entry
                    .name
                    .to_str()
                    .map(|name| pattern.matches(name))
                    .unwrap_or(false);
                if matched {
                    local += 1;
                }
            }
            EntryKind::Dir => {
                let child = dir.join(&entry.name);
                scope.spawn(move |inner| count_dir(child, pattern, count, inner));
            }
            EntryKind::Skip => {}
        }
    }
    if local > 0 {
        count.fetch_add(local, Ordering::Relaxed);
    }
}

/// Size every immediate child of `root`, returning `(total, children desc)`.
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

// ---------------------------------------------------------------------------
// Portable fallback (always compiled; the macOS bulk path falls back to this
// when DEEPSCAN_NO_BULK is set, which also enables A/B benchmarking).
// ---------------------------------------------------------------------------

fn read_dir_lite_std(dir: &Path) -> Vec<LiteEntry> {
    let read_dir = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(_) => return Vec::new(),
    };

    let mut entries = Vec::new();
    for entry in read_dir.filter_map(Result::ok) {
        let meta = match fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            EntryKind::Skip
        } else if file_type.is_file() {
            EntryKind::File(meta.len())
        } else if file_type.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::Skip
        };
        entries.push(LiteEntry {
            name: entry.file_name(),
            kind,
        });
    }
    entries
}

// ---------------------------------------------------------------------------
// macOS fast path — getattrlistbulk(2)
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "macos"))]
#[inline]
fn read_dir_lite(dir: &Path) -> Vec<LiteEntry> {
    read_dir_lite_std(dir)
}

#[cfg(target_os = "macos")]
fn read_dir_lite(dir: &Path) -> Vec<LiteEntry> {
    use std::sync::OnceLock;
    static USE_BULK: OnceLock<bool> = OnceLock::new();
    let use_bulk = *USE_BULK.get_or_init(|| std::env::var_os("DEEPSCAN_NO_BULK").is_none());
    if use_bulk {
        mac::read_dir_bulk(dir)
    } else {
        read_dir_lite_std(dir)
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CString, OsStr, OsString};
    use std::os::raw::c_int;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::{EntryKind, LiteEntry};

    // struct attrlist — sys/attr.h
    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0002;

    // enum vtype — sys/vnode.h
    const VREG: u32 = 1;
    const VDIR: u32 = 2;

    extern "C" {
        fn getattrlistbulk(
            dirfd: c_int,
            attr_list: *mut AttrList,
            attr_buf: *mut libc::c_void,
            attr_buf_size: libc::size_t,
            options: u64,
        ) -> c_int;
    }

    #[inline]
    fn rd_u32(buf: &[u8], off: usize) -> u32 {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&buf[off..off + 4]);
        u32::from_ne_bytes(bytes)
    }

    #[inline]
    fn rd_i32(buf: &[u8], off: usize) -> i32 {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&buf[off..off + 4]);
        i32::from_ne_bytes(bytes)
    }

    #[inline]
    fn rd_u64(buf: &[u8], off: usize) -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&buf[off..off + 8]);
        u64::from_ne_bytes(bytes)
    }

    pub(super) fn read_dir_bulk(dir: &Path) -> Vec<LiteEntry> {
        let c_path = match CString::new(dir.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => return Vec::new(),
        };

        // Open the directory itself (we only ever recurse into VDIR entries,
        // so this is never a symlink we'd be following).
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
        if fd < 0 {
            return Vec::new();
        }

        let mut attr_list = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE,
            volattr: 0,
            dirattr: 0,
            fileattr: ATTR_FILE_TOTALSIZE,
            forkattr: 0,
        };

        let mut entries = Vec::new();
        let mut buf = vec![0u8; 256 * 1024];

        loop {
            let count = unsafe {
                getattrlistbulk(
                    fd,
                    &mut attr_list,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    0,
                )
            };
            if count <= 0 {
                break; // 0 = done, <0 = error
            }

            let mut offset = 0usize;
            for _ in 0..count {
                if offset + 24 > buf.len() {
                    break;
                }
                let entry = &buf[offset..];

                // Layout: u32 length, then attribute_set_t returned (5 x u32),
                // then each requested attr in bitmap order *if* it was returned.
                let length = rd_u32(entry, 0) as usize;
                let returned_common = rd_u32(entry, 4);
                let returned_file = rd_u32(entry, 16);
                let mut cursor = 24usize; // 4 (length) + 20 (returned set)

                let mut name: Option<OsString> = None;
                if returned_common & ATTR_CMN_NAME != 0 {
                    // attrreference_t { i32 dataoffset, u32 length }, offset is
                    // relative to the attrreference_t's own address.
                    let data_offset = rd_i32(entry, cursor) as isize;
                    let name_len = rd_u32(entry, cursor + 4) as usize;
                    let name_start = cursor as isize + data_offset;
                    if name_len > 0
                        && name_start >= 0
                        && (name_start as usize) + name_len <= entry.len()
                    {
                        let raw = &entry[name_start as usize..name_start as usize + name_len];
                        let trimmed = raw.split(|&b| b == 0).next().unwrap_or(raw);
                        name = Some(OsString::from(OsStr::from_bytes(trimmed)));
                    }
                    cursor += 8; // sizeof(attrreference_t)
                }

                let mut obj_type = 0u32;
                if returned_common & ATTR_CMN_OBJTYPE != 0 {
                    obj_type = rd_u32(entry, cursor);
                    cursor += 4;
                }

                let mut size = 0u64;
                let mut have_size = false;
                if returned_file & ATTR_FILE_TOTALSIZE != 0 {
                    size = rd_u64(entry, cursor);
                    have_size = true;
                    cursor += 8;
                }
                let _ = cursor;

                if let Some(name) = name {
                    let kind = match obj_type {
                        VREG => EntryKind::File(if have_size { size } else { 0 }),
                        VDIR => EntryKind::Dir,
                        _ => EntryKind::Skip, // symlinks, devices, fifos, sockets
                    };
                    entries.push(LiteEntry { name, kind });
                }

                if length == 0 {
                    break; // guard against a malformed record
                }
                offset += length;
            }
        }

        unsafe {
            libc::close(fd);
        }
        entries
    }
}
