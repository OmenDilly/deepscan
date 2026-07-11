# deepscan Interactive Phase 4 — the guided multi-source `clean` hub

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Bare interactive `deepscan clean` (no `--only`, no `--apply`, on a TTY) becomes a guided hub that surfaces **caches + duplicates + old large files** in one select→Trash list — instead of caches only. `--only`, `--apply`, `--json`, `--plain`, and piped `clean` are unchanged (caches-only, per the Phase-1 safety decision that headless never auto-Trashes dupes/old files).

**Architecture:** A localized change to `run_clean`'s existing interactive branch in `crates/deepscan-cli/src/main.rs`. When `only` is empty, additionally gather `find_duplicates` (grouped, protect-one-copy via `group: Some(gid)`) and old `find_large_files`, append them to the cache rows, and run the same shared `tui::run_list`. Reuses everything from Phases 1–3; no new modules; `deepscan-core` unchanged.

**Tech Stack:** Rust 2021, the Phase-1 shared sized-list widget + `report_trash`.

## Global Constraints

- Toolchain 1.96.0; ends fmt-clean, `cargo clippy --all-targets -- -D warnings` clean, all tests passing.
- Only the **bare interactive** path changes. `clean --only …`, `clean --apply`, `clean --json`, `clean --plain`, and piped `clean` are byte-identical to today (caches only). The `reclaim` alias is unaffected.
- Deletion still routes through `tui::run_list` → the guarded `trash_selected` (`is_safe_to_trash`) + confirm modal; `report_trash` surfaces failures. Duplicates keep one copy per set (widget `group` protect-last). No headless path ever auto-Trashes dupes/old files.
- Keep the hub responsive: dupes at a **10 MB** floor (bounds hashing), old files at **≥100 MB and ≥90 days**, each capped (40 sets / 40 files). The walk runs under `with_spinner`.

---

### Task 1: Make bare interactive `clean` the caches + dupes + old-files hub

**Files:**
- Modify: `crates/deepscan-cli/src/main.rs` (`run_clean` interactive branch, ~lines 1070-1081)

**Interfaces:**
- Consumes: `find_duplicates`, `find_large_files`, `with_spinner`, `home`, `home_dir`, `tui::{Row, RowMeta, Sort, run_list, ListConfig}`, `report_trash` (all already in main.rs).

- [ ] **Step 1: Replace the caches-only interactive branch with the hub**

The current branch (after `rows` is built from `plan.auto_targets` + `plan.manual_targets`) is:
```rust
    if !apply && interactive(json, plain) && !rows.is_empty() {
        let outcome = tui::run_list(
            rows,
            tui::Sort::Size,
            tui::ListConfig {
                title: "deepscan clean · caches".to_string(),
                home: home_dir(),
            },
        )?;
        report_trash(&outcome);
        return Ok(());
    }
```

Replace it with:
```rust
    if !apply && interactive(json, plain) {
        // Bare `clean` (no --only) is the guided hub: caches + duplicates + old
        // large files in one select→Trash list. `--only` stays caches-only.
        // Headless clean (--apply/--json/--plain/piped) never gathers these —
        // a script must not auto-Trash duplicates or old files.
        if only.is_empty() {
            let walk_home = home();
            let (dupes, olds) = with_spinner("scanning duplicates + old files", move || {
                let dupes = find_duplicates(&walk_home, 10 * 1024 * 1024);
                let mut olds = find_large_files(&walk_home, 100 * 1024 * 1024);
                olds.retain(|f| f.modified_days.map(|d| d >= 90).unwrap_or(false));
                olds.truncate(40);
                (dupes, olds)
            });
            for (gid, group) in dupes.iter().take(40).enumerate() {
                for path in &group.paths {
                    rows.push(tui::Row {
                        bytes: group.bytes,
                        label: path.display().to_string(),
                        path: path.clone(),
                        selected: false,
                        meta: tui::RowMeta::Tag(format!("dup×{}", group.paths.len())),
                        group: Some(gid),
                    });
                }
            }
            rows.extend(olds.iter().map(|f| {
                tui::Row::new(
                    f.bytes,
                    f.path.display().to_string(),
                    f.path.clone(),
                    tui::RowMeta::Age(f.modified_days),
                )
            }));
        }
        if !rows.is_empty() {
            let title = if only.is_empty() {
                "deepscan clean · caches + duplicates + old files".to_string()
            } else {
                "deepscan clean · caches".to_string()
            };
            let outcome = tui::run_list(
                rows,
                tui::Sort::Size,
                tui::ListConfig { title, home: home_dir() },
            )?;
            report_trash(&outcome);
            return Ok(());
        }
    }
```

Notes for the implementer:
- Cache rows have `group: None` (via `tui::Row::new`); only dupe copies carry `group: Some(gid)`, so the widget's protect-last rule keeps one copy per set and never touches caches/old files.
- If everything is empty (no caches, no dupes, no old files), the branch falls through to the existing dry-run print below — unchanged.
- `home()` returns `PathBuf` (already defined in main.rs); it's moved into the spinner closure. `only.is_empty()` is still available afterward (only borrowed earlier by `filter_plan`).

- [ ] **Step 2: Build + smoke-test (hub + unchanged headless paths)**

```bash
cd /Users/dmitrijacenko/projects/deepscan
cargo build -q --release -p deepscan-cli
# headless caches-only paths UNCHANGED (fast, no dupe/old-file walk):
./target/release/deepscan clean --plain 2>/dev/null | head -1           # header "deepscan clean · dry run"
./target/release/deepscan clean --json 2>/dev/null | python3 -c "import json,sys;json.load(sys.stdin);print('clean json ok')"
./target/release/deepscan reclaim --plain 2>/dev/null | head -1         # alias unchanged
# --only interactive stays caches-only (fast); feed q after the reclaim spinner:
( sleep 2; printf 'q' ) | script -q /dev/null ./target/release/deepscan clean --only npm >/dev/null 2>&1 && echo "clean --only ok"
# bare hub launches (gathers caches+dupes+old files — allow time for the walk, then q):
( sleep 15; printf 'q' ) | timeout 90 script -q /dev/null ./target/release/deepscan clean >/dev/null 2>&1 && echo "clean hub ok"
```
Expected: the plain/json/alias headers unchanged + valid; "clean --only ok"; "clean hub ok" (the hub launches, lists caches+dupes+old files, quits restoring the terminal). If the bare-hub home walk is slow on this machine, increase the sleep; the point is it launches and quits cleanly.

- [ ] **Step 3: clippy + full test + commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test -q
git add -A && git commit -m "feat(clean): bare interactive clean is a caches+dupes+old-files hub"
```

---

## Phase 4 exit criteria

- `deepscan clean` on a terminal (no `--only`/`--apply`) opens a select→Trash list combining reclaimable caches (safe/review), duplicate sets (protecting one copy each), and old large files (≥100 MB, ≥90 days) — one confirm, failures surfaced.
- `clean --only`, `clean --apply`, `clean --json`, `clean --plain`, piped `clean`, and the `reclaim` alias are byte-identical to before (caches only); no headless path auto-Trashes dupes/old files.
- fmt/clippy `-D warnings`/tests clean; CI green.

This completes the interactive redesign (Phases 1–4).
