//! Side-effecting actions for the interactive views: move selected items to the
//! Trash (guarded by `is_safe_to_delete`), and reveal a path in Finder. The
//! Trash call is injectable so the guard is unit-tested without touching Trash.

use std::path::{Path, PathBuf};
use std::process::Command;

use deepscan_core::{is_safe_to_delete, move_to_trash};

#[derive(Debug, Default)]
pub struct TrashOutcome {
    pub freed_bytes: u64,
    pub failures: Vec<(PathBuf, String)>,
}

/// Trash each `(path, bytes)`, refusing any path that fails the safety guard.
pub fn trash_selected(items: &[(PathBuf, u64)], home: Option<&Path>) -> TrashOutcome {
    trash_selected_with(items, home, move_to_trash)
}

pub fn trash_selected_with(
    items: &[(PathBuf, u64)],
    home: Option<&Path>,
    delete: impl Fn(&Path) -> Result<(), String>,
) -> TrashOutcome {
    let mut outcome = TrashOutcome::default();
    for (path, bytes) in items {
        if !is_safe_to_delete(path, home) {
            outcome
                .failures
                .push((path.clone(), "refused: failed safety guard".into()));
            continue;
        }
        match delete(path) {
            Ok(()) => outcome.freed_bytes += bytes,
            Err(err) => outcome.failures.push((path.clone(), err)),
        }
    }
    outcome
}

/// Open Finder with `path` selected. Best-effort; failures are ignored.
pub fn reveal_in_finder(path: &Path) {
    let _ = Command::new("open").arg("-R").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_refuses_dangerous_paths_and_trashes_safe_ones() {
        let home = PathBuf::from("/Users/tester");
        let items = vec![
            (PathBuf::from("/Users/tester/Library/Caches/foo"), 100u64),
            (PathBuf::from("/"), 0u64), // must be refused
        ];
        let deleted = std::cell::RefCell::new(Vec::new());
        let outcome = trash_selected_with(&items, Some(&home), |p| {
            deleted.borrow_mut().push(p.to_path_buf());
            Ok(())
        });
        assert_eq!(outcome.freed_bytes, 100);
        assert_eq!(outcome.failures.len(), 1, "root refused");
        assert_eq!(deleted.borrow().len(), 1, "only the safe path was deleted");
    }
}
