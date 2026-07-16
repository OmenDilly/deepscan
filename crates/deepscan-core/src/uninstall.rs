//! Heuristic app uninstaller (the Pearcleaner blueprint). Resolve an app to its
//! bundle id, then glob the known macOS Library locations for support files
//! whose name carries that bundle id (high confidence) or the app's name
//! (medium — review first). deepscan never hard-deletes here: dry-run by
//! default, and `--apply` moves everything to the **Trash** (recoverable).
//!
//! A heuristic matcher reaches AppCleaner/Pearcleaner accuracy — it matches the
//! common case and is honest (confidence tiers) about the rest. It will not
//! find leftovers that embed neither the bundle id nor the app name.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::engine::{home_dir, is_safe_to_trash};
use crate::report::{Confidence, Leftover, UninstallOutcome, UninstallPlan};
use crate::scan::measure_path;

/// The per-user Library locations apps scatter support files across.
const LIBS: &[&str] = &[
    "~/Library/Application Support",
    "~/Library/Caches",
    "~/Library/Preferences",
    "~/Library/Containers",
    "~/Library/Group Containers",
    "~/Library/Logs",
    "~/Library/Saved Application State",
    "~/Library/WebKit",
    "~/Library/HTTPStorages",
    "~/Library/Cookies",
    "~/Library/LaunchAgents",
];

/// Build the uninstall plan for an app name or `.app` path. Returns `None` if
/// neither an app bundle nor a bundle id can be resolved.
pub fn plan_uninstall(query: &str) -> Option<UninstallPlan> {
    let app_path = find_app(query);
    let app_name = app_path
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| query.to_string());
    let bundle_id = app_path.as_ref().and_then(|p| read_bundle_id(p));

    if app_path.is_none() && bundle_id.is_none() {
        return None;
    }

    let app_bytes = app_path.as_ref().map(|p| measure_path(p)).unwrap_or(0);
    let leftovers = find_leftovers(&app_name, bundle_id.as_deref());
    let total_bytes = app_bytes + leftovers.iter().map(|l| l.bytes).sum::<u64>();

    Some(UninstallPlan {
        app_name,
        bundle_id,
        app_path,
        app_bytes,
        leftovers,
        total_bytes,
    })
}

/// Move the app bundle + every leftover to the Trash (recoverable).
pub fn execute_uninstall(plan: &UninstallPlan) -> Vec<UninstallOutcome> {
    let mut targets: Vec<(PathBuf, u64)> = Vec::new();
    if let Some(app) = &plan.app_path {
        targets.push((app.clone(), plan.app_bytes));
    }
    for leftover in &plan.leftovers {
        targets.push((leftover.path.clone(), leftover.bytes));
    }

    let home = home_dir();
    targets
        .into_iter()
        .map(|(path, bytes)| {
            if !is_safe_to_trash(&path, home.as_deref()) {
                return UninstallOutcome {
                    path,
                    bytes,
                    ok: false,
                    error: Some("refused: failed safety guard".to_string()),
                };
            }
            match trash::delete(&path) {
                Ok(()) => UninstallOutcome {
                    path,
                    bytes,
                    ok: true,
                    error: None,
                },
                Err(err) => UninstallOutcome {
                    path,
                    bytes,
                    ok: false,
                    error: Some(err.to_string()),
                },
            }
        })
        .collect()
}

fn find_app(query: &str) -> Option<PathBuf> {
    let direct = Path::new(query);
    if direct
        .extension()
        .map(|e| e.eq_ignore_ascii_case("app"))
        .unwrap_or(false)
        && direct.exists()
    {
        return Some(direct.to_path_buf());
    }

    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = home_dir() {
        dirs.push(home.join("Applications"));
    }
    let query = query.to_lowercase();
    let mut best: Option<(PathBuf, usize)> = None;
    for dir in dirs {
        let read_dir = match fs::read_dir(&dir) {
            Ok(read_dir) => read_dir,
            Err(_) => continue,
        };
        for entry in read_dir.filter_map(Result::ok) {
            let path = entry.path();
            if !path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("app"))
                .unwrap_or(false)
            {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if stem == query {
                return Some(path); // exact match wins immediately
            }
            if stem.contains(&query) && best.as_ref().map(|(_, l)| stem.len() < *l).unwrap_or(true)
            {
                best = Some((path, stem.len()));
            }
        }
    }
    best.map(|(path, _)| path)
}

/// Lowercased names **and** bundle ids of every installed app in
/// `/Applications` and `~/Applications` — the vocabulary a baseline-worthy
/// folder must match. A folder with no installed app behind it is either a
/// machine-local artifact or an orphaned leftover; neither is a typical size
/// for anyone else.
pub fn installed_app_keys() -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = home_dir() {
        dirs.push(home.join("Applications"));
    }
    for dir in dirs {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.filter_map(Result::ok) {
            let path = entry.path();
            if !path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("app"))
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                keys.insert(stem.to_lowercase());
            }
            if let Some(id) = read_bundle_id(&path) {
                keys.insert(id.to_lowercase());
            }
        }
    }
    keys
}

fn read_bundle_id(app: &Path) -> Option<String> {
    let info = app.join("Contents/Info");
    let output = Command::new("defaults")
        .arg("read")
        .arg(&info)
        .arg("CFBundleIdentifier")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!id.is_empty()).then_some(id)
}

fn find_leftovers(app_name: &str, bundle_id: Option<&str>) -> Vec<Leftover> {
    let name_q = app_name.to_lowercase();
    let bid_q = bundle_id.map(str::to_lowercase);

    let mut out = Vec::new();
    for lib in LIBS {
        let dir = PathBuf::from(shellexpand::tilde(lib).into_owned());
        let read_dir = match fs::read_dir(&dir) {
            Ok(read_dir) => read_dir,
            Err(_) => continue,
        };
        for entry in read_dir.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let confidence = match &bid_q {
                Some(bid) if name.contains(bid.as_str()) => Some(Confidence::High),
                _ if name.starts_with(name_q.as_str()) => Some(Confidence::Medium),
                _ => None,
            };
            if let Some(confidence) = confidence {
                let path = entry.path();
                let bytes = measure_path(&path);
                out.push(Leftover {
                    path,
                    bytes,
                    confidence,
                    location: lib.trim_start_matches("~/Library/").to_string(),
                });
            }
        }
    }
    out.sort_by_key(|leftover| std::cmp::Reverse(leftover.bytes));
    out
}
