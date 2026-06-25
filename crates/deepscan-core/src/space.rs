//! Honest disk-space accounting — the "System Data" wedge.
//!
//! Reports a volume's true capacity (`statfs`) and lists APFS local snapshots.
//! macOS does **not** expose per-snapshot sizes via `tmutil` or `diskutil`
//! (verified), so deepscan reports what *is* knowable — total/used/free + the
//! snapshot list + the reclaim command — instead of inventing a number. That
//! honesty is the differentiator: every other tool either hides this or fakes
//! a "System Data" figure that is, in Apple's own behavior, a guesstimate.

use std::path::Path;

use crate::report::{DiskSpace, SpaceReport};

/// True capacity of the filesystem containing `path`, via `statfs(2)`.
#[cfg(target_os = "macos")]
pub fn disk_space(path: &Path) -> Option<DiskSpace> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buf = MaybeUninit::<libc::statfs>::uninit();
    let rc = unsafe { libc::statfs(c_path.as_ptr(), buf.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let stat = unsafe { buf.assume_init() };

    let bsize = stat.f_bsize as u64;
    let total = stat.f_blocks * bsize;
    let free = stat.f_bavail * bsize; // available to this user
    let used = total.saturating_sub(stat.f_bfree * bsize); // matches `df` used

    Some(DiskSpace {
        path: path.to_path_buf(),
        total,
        used,
        free,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn disk_space(_path: &Path) -> Option<DiskSpace> {
    None
}

/// APFS local snapshot names for a mount point, via `tmutil`. Names only —
/// macOS does not publish per-snapshot sizes.
pub fn local_snapshots(mount: &str) -> Vec<String> {
    let output = match std::process::Command::new("tmutil")
        .arg("listlocalsnapshots")
        .arg(mount)
        .output()
    {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("com.apple"))
        .map(str::to_string)
        .collect()
}

/// Assemble the honest space report for the volume containing `path`.
pub fn space_report(path: &Path) -> SpaceReport {
    let disk = disk_space(path);
    let snapshots = local_snapshots("/");
    // Aggressive thinning frees as much purgeable snapshot space as macOS will
    // allow; we only ever *print* this — never run it.
    let reclaim_snapshots = if snapshots.is_empty() {
        None
    } else {
        Some("sudo tmutil thinlocalsnapshots / 999999999999 4".to_string())
    };

    SpaceReport {
        disk,
        snapshots,
        reclaim_snapshots,
    }
}
