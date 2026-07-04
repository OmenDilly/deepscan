# deepscan CLI redesign — interactive-everywhere, merged by job

**Date:** 2026-07-04
**Status:** Approved design, pre-implementation
**Scope:** `deepscan-cli` (the interaction layer). `deepscan-core` is the data layer and does not change except where noted (routing deletes through Trash).

## 1. Context & problem

deepscan today has 8 print-only subcommands: `scan`, `anomalies`, `reclaim`,
`space`, `large`, `dupes`, `uninstall`, `explore`. Only `explore` is
interactive. Two problems:

1. **Redundancy.** The commands serve two jobs — *SEE* (understand where space
   went) and *ACT* (reclaim it) — with real overlap. `space`'s disk line
   already lives inside `scan`; `scan`/`anomalies`/`large`/`explore` are four
   "show big things" lenses; `dupes` lists waste but cannot act on it.
2. **Inconsistent interaction & safety.** Everything but `explore` is
   print-only. Deletion is split two ways: `reclaim` permanently `rm -rf`s,
   while `uninstall` moves to Trash.

Every SEE command already emits a **list or tree of sized items** — the exact
shape `explore` makes interactive. That is the leverage point for this redesign.

## 2. Locked decisions

From the brainstorming session:

1. **Interaction = auto (TTY-aware).** A command run in a real terminal opens an
   interactive view; piped/redirected/`--json` output stays plain text. Scripts
   and CI are untouched. (deepscan already auto-switches color/spinner this way.)
2. **Merge = medium (by job).** Consolidate the SEE lenses under `scan` +
   `explore` + focused finders; consolidate ACT under `clean` + `uninstall`.
3. **Actions = select → Trash + confirm.** Interactive views browse read-only
   until you multi-select and confirm; deletion moves items to the macOS Trash
   (recoverable). Everything standardizes on Trash — no more permanent `rm -rf`.
4. **Shared widget.** One reusable interactive "sized-list with select → Trash"
   component backs `large`, `dupes`, cache-reclaim, and `uninstall`; `clean`
   with no args is a guided hub that composes the same widget over multiple
   sources. No duplicated interaction logic.

Back-compat choices (approved):

5. `space` and `anomalies` are **kept as aliases** that jump into the
   corresponding `scan` view — non-breaking, preserves muscle memory and the
   recent anomaly-classification work.
6. `reclaim` is **renamed `clean`**, with `reclaim` kept as a hidden alias.

## 3. Architecture

```
deepscan-core (data, unchanged)          deepscan-cli (all new code here)
  find_large_files ─┐
  find_duplicates ──┤                    tui/widget.rs   SizedList + Tree state machines
  evaluate_catalog ─┼──► Vec<Row> ──►    tui/action.rs   Trash + confirm modal + reveal
  plan_uninstall ───┤                    tui/render.rs   ratatui draw for widget + dashboard
  detect_anomalies ─┤                    tui/mod.rs      run_list / run_tree / run_dashboard
  space_report ─────┘                    main.rs         adapters + plain/--json printers
```

- **Core is the data layer.** It already returns the structured lists each
  command needs. The only core change: route reclaim deletion through Trash
  (see §7).
- **One TUI layer** in `deepscan-cli` owns all interaction. Commands are thin
  adapters: build rows (or a `ScanReport`/tree), then call the matching
  `run_*`, or print the plain/JSON form.
- **Auto-TTY dispatch** (in each command handler):
  `if stdout.is_terminal() && !json && !plain { run_interactive(...) } else { print_plain(...) }`.
  A `--plain` flag forces the printed form; `-i/--interactive` forces the TUI
  even when piped (rare, for demos).

## 4. The shared sized-list widget

A flat, navigable list of rows. One row = `{ selected: bool, bytes: u64,
label: String, path: PathBuf, meta: RowMeta }` where `meta` carries per-source
extras (age for `large`, confidence for `uninstall`, group id for `dupes`,
safe/review for caches).

**State (pure, unit-testable — no terminal):**
```
struct ListState {
    rows: Vec<Row>,
    cursor: usize,
    sort: Sort,          // Size | Age | Name
    // selection lives in Row.selected
}
```
Operations (all pure, tested like `explore`'s navigation today):
`move_up/down`, `toggle`, `select_all`, `select_none`, `set_sort` (re-sorts,
keeps cursor on the same row), `selected_bytes()`, `selected_paths()`.

**Keymap:** `↑↓`/`jk` move · `space` toggle · `a` all · `n` none ·
`s` cycle sort · `d`/`Enter` Trash selection · `f` reveal-in-Finder · `q`/`Esc`
quit.

**Render** (thin, over `ListState`): bar + size + per-source column (age /
tag / confidence) + path; header shows source label + totals; footer shows
`selected: N · X GB` and the keymap. Reuses the existing `explore` bar/size
style.

**Layout sketch:**
```
 deepscan large · ~/  · 41.2 GB in 18 files over 100 MB          [selected: 2 · 23.5 GB]
 ────────────────────────────────────────────────────────────────────────────────────
 ▶ [x] ███████████████  13.5 GB   792d  Documents/leaks.trace/…/event_data_17134.oa
   [x] ███████████░░░░  10.0 GB     4d  Library/…/Claude/vm_bundles/…/sessiondata.img
   [ ] ██████░░░░░░░░░   6.1 GB     5d  .ollama/models/blobs/sha256-dec52a44…
 ────────────────────────────────────────────────────────────────────────────────────
 ↑↓ move  space select  a all  n none  s sort  d Trash  f reveal  q quit
```

## 5. The tree widget (`explore`)

The existing `explore` tree, extended with the shared action layer: `space`
toggles the highlighted node, `d` Trashes selected nodes (with confirm), `f`
reveals. Navigation/streaming-sizing behavior is unchanged. Selection + action
reuse `tui/action.rs`.

## 6. Command-by-command behavior

| Command | Interactive (TTY) | Plain / `--json` |
|---|---|---|
| `scan [PATH]` | Dashboard (see below) | Today's sections + folded-in space accounting + anomalies section; `--json` carries all |
| `explore [PATH]` | Tree + select→Trash + reveal | n/a (interactive-only; prints a hint if piped) |
| `large [PATH] [--older N] [--min-mb M] [--top N]` | SizedList over `find_large_files` | Today's list; `--json` |
| `dupes [PATH] [--min-mb M] [--top N]` | SizedList grouped by set; **protects last copy** | Today's list; `--json` |
| `clean [--only P…] [--apply --yes]` | No-arg → guided hub (caches → dupes → old files sections of the SizedList, one final confirm). `--only` → filtered cache list | Today's `reclaim` dry-run/apply; `--json` |
| `uninstall <APP> [--apply --yes]` | SizedList of app + leftovers, pre-selected, toggle → Trash | Today's plan; `--json` |
| `space [PATH]` | **Alias** → scan accounting view | Today's `space` output; `--json` |
| `anomalies [PATH]` | **Alias** → scan app-data/caches section | Today's `anomalies` output; `--json` |

### `scan` dashboard (interactive)

A navigable summary; `Enter` on a section/row drills into the matching focused
view (which is the same shared widget):

```
 deepscan · ~/dmitrijacenko                        357.3 GB used of 460.4 GB · 103.2 free
 Disk: ▓▓▓▓▓▓▓▓▓▓▓▓▓░░░  + 3 local snapshots (System Data — deepscan space)
 ────────────────────────────────────────────────────────────────────────────────────
 RECLAIMABLE     44.2 GB safe · 25.9 GB review        → Enter: open clean
   25.3 GB [safe]   Xcode DerivedData
   11.4 GB [safe]   User caches
 APP DATA        review before removing               → Enter: open explore here
   20.7 GB [CRIT]   Claude
 BIGGEST FILES                                        → Enter: open large
   13.5 GB          Documents/leaks.trace/…/event_data_17134.oa
 LEAKS           1 warning
   25.3 GB [WARN]   Xcode DerivedData bloat
```

Drill-in launches the corresponding `run_list`/`run_tree` in place; on quit you
return to the dashboard.

## 7. Safety model

- **All deletions → Trash** (recoverable) via the `trash` crate. `clean` stops
  calling `std::fs::remove_dir_all`; `execute_reclaim` is rerouted through Trash
  (align with `uninstall`'s existing `trash::delete`). This is the one core
  change.
- **Confirm modal** before any Trash: shows count + total size + the item list;
  `y/N`. Applies to every interactive view and the tree.
- **Keep `is_safe_to_delete`** (refuses relative paths, `..`, `/`, near-root,
  the home dir itself). Checked before Trashing every path.
- **`dupes` protects the last copy** of every set — the widget refuses to
  select all copies of a group; at least one is always kept.
- **Scripting unchanged:** `clean --apply --yes`, `--only`, and `--json` run
  headless with no TUI. Non-interactive delete also goes to Trash now (safer
  default; document the change).
- **The guided hub is interactive-only.** Headless `clean` / `clean --apply`
  operates on **caches only** (today's `reclaim` behavior) for predictable
  scripting. The multi-source sweep (caches → dupes → old files) is a
  convenience of the interactive no-arg view, where every deletion is
  explicitly selected and confirmed — a script never auto-Trashes duplicates or
  old files.

## 8. Module structure

```
deepscan-cli/src/
  main.rs            command parse + adapters + plain/JSON printers + auto-TTY dispatch
  tui/
    mod.rs           run_list(rows, cfg) · run_tree(root) · run_dashboard(report)
    widget.rs        ListState (pure) + Tree state (moved from tui.rs) + Sort
    action.rs        Trash + confirm modal + reveal-in-Finder + is_safe_to_delete hookup
    render.rs        ratatui draw for list, tree, dashboard, confirm modal
```

The current `tui.rs` (explore) is split: its pure navigation → `widget.rs`, its
draw → `render.rs`, its action wiring → `action.rs`. Each file has one job and
stays small enough to hold in context.

## 9. Non-goals (YAGNI)

Not in this redesign: permanent-delete, keep/exclude lists, config files,
cross-machine/community baselines, temporal self-baselines, a Swift GUI. Sort
is size/age/name only. No mouse support.

## 10. Phasing

1. **Shared `SizedList`** widget (pure state + render + action layer) and wire
   `large`, `dupes`, `clean`, `uninstall`. Reroute reclaim delete → Trash.
2. **`scan` dashboard** (folds in `space` accounting + `anomalies` section +
   biggest-files section; drill-in to the focused views).
3. **`explore`** gains select → Trash + reveal (reuse `action.rs`).
4. **`space` / `anomalies` aliases**; `reclaim` → `clean` rename + alias.

Each phase is independently shippable and leaves the CLI working.

## 11. Testing strategy

- **Pure state machines** (`ListState`, tree nav, sort-keeps-cursor, dupes
  protect-last, confirm-flow gating) are unit-tested with no terminal — the
  pattern `explore` already uses.
- **Trash action** takes an injectable deleter (like `execute_reclaim(home:)`
  today) so tests assert "safe target trashed, guarded path refused" against a
  temp fixture without touching the real Trash.
- **Adapters/core** already covered by existing tests; plain-print and `--json`
  paths keep their current output (regression-checked by eye + JSON validity).
- **TUI lifecycle** smoke-tested via a faked TTY (`script -q /dev/null` + piped
  `q`) — launches and restores the terminal cleanly. The agent cannot drive the
  interactive UI, so correctness rides on the pure state machines.
