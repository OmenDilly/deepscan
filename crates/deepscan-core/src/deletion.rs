//! The single deletion path: move to the macOS Trash (recoverable). Everything
//! that removes files — `reclaim`/`clean`, `uninstall`, and the interactive
//! views — routes through here so behavior and safety are uniform.

use std::path::Path;

/// Move `path` to the Trash. Returns a human-readable error on failure.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trashes_a_real_file() {
        let base = std::env::temp_dir().join(format!("deepscan-trash-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let victim = base.join("junk.bin");
        std::fs::write(&victim, vec![0u8; 32]).unwrap();

        move_to_trash(&victim).expect("trash should succeed");
        assert!(!victim.exists(), "file left its original location");

        let _ = std::fs::remove_dir_all(&base);
    }
}
