# deepscan `growth` — temporal self-baseline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A `deepscan growth` command that snapshots the anomaly-zone folder sizes each run, compares to the most recent prior snapshot in `~/.deepscan/history.json`, and reports folders that grew abnormally fast (or appeared and ballooned) — turning "big" into "abnormal vs its own past".

**Architecture:** New `deepscan-core/src/baseline.rs` holds the snapshot/history/growth logic (pure `compute_growth` + IO), reusing `anomaly::{default_zones, sized_children, classify}` and `signatures::expand_paths`. A new CLI `growth` command records + compares + prints. Standalone — does not touch `scan`'s fast path or any other command.

**Tech Stack:** Rust 2021, serde + serde_json (new core dep), the existing anomaly zones.

## Global Constraints

- Toolchain 1.96.0; every task ends fmt-clean, `cargo clippy --all-targets -- -D warnings` clean, all tests passing.
- History file: `~/.deepscan/history.json`, a JSON array of `{ taken: <unix secs>, sizes: { "<path>": <bytes> } }`, newest last, capped at 30 (oldest pruned). Tolerant of missing/corrupt (start fresh). Created on first run.
- Growth thresholds (exact): flag a grown folder when `delta ≥ 500 MB` AND `delta/previous ≥ 0.25`; flag a new folder when `current ≥ 500 MB`. Rank by `delta` desc.
- No TUI, no background recording, no touching other commands. `--json` emits the `GrowthReport`.
- Tests must NOT write to the real `~/.deepscan` — test the pure `compute_growth` and a serde round-trip against a temp file only.

---

### Task 1: Core `baseline.rs` — snapshot, history, growth

**Files:**
- Modify: `crates/deepscan-core/Cargo.toml` (add `serde_json`)
- Modify: `crates/deepscan-core/src/anomaly.rs` (expose `classify`, `sized_children` as `pub(crate)`)
- Create: `crates/deepscan-core/src/baseline.rs`
- Modify: `crates/deepscan-core/src/lib.rs` (`pub mod baseline;` + re-exports)

**Interfaces:**
- Produces: `Snapshot { taken: u64, sizes: BTreeMap<String, u64> }`, `Growth { path, name, kind: AnomalyKind, current, previous: Option<u64>, delta, pct: Option<f64>, days, is_new }`, `GrowthReport { previous_at: Option<u64>, now: u64, entries: Vec<Growth>, baseline_only: bool }`
- Produces: `compute_growth(prev: &Snapshot, current: &Snapshot) -> Vec<Growth>` (pure), `snapshot_now() -> Snapshot`, `load_history()`/`save_history(&[Snapshot])`, `record_and_compare() -> GrowthReport`.

- [ ] **Step 1: Add the `serde_json` dependency**

In `crates/deepscan-core/Cargo.toml`, under `[dependencies]`, add:
```toml
serde_json = "1"
```

- [ ] **Step 2: Expose the two anomaly helpers as `pub(crate)`**

In `crates/deepscan-core/src/anomaly.rs`, change the two private fns' visibility (bodies unchanged):
- `fn classify(path: &Path) -> AnomalyKind {` → `pub(crate) fn classify(path: &Path) -> AnomalyKind {`
- `fn sized_children(container: &Path) -> Vec<(PathBuf, u64)> {` → `pub(crate) fn sized_children(container: &Path) -> Vec<(PathBuf, u64)> {`

- [ ] **Step 3: Write `baseline.rs` with the pure logic + tests**

Create `crates/deepscan-core/src/baseline.rs`:

```rust
//! Temporal self-baseline — the local half of real anomaly detection. Each run
//! snapshots the sizes of the anomaly-zone folders, compares to the most recent
//! prior snapshot, and reports what grew abnormally fast (or appeared and
//! ballooned). History lives in `~/.deepscan/history.json` (rolling, last 30).
//! This turns "big" into "abnormal vs its own past".

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::anomaly::{classify, default_zones, sized_children, AnomalyKind};
use crate::engine::home_dir;
use crate::signatures::expand_paths;

const MB: u64 = 1024 * 1024;
/// Minimum absolute growth to flag.
const GROWTH_FLOOR: u64 = 500 * MB;
/// Minimum relative growth to flag (25%).
const GROWTH_PCT: f64 = 0.25;
/// Rolling history cap.
const MAX_HISTORY: usize = 30;

/// A point-in-time record of the anomaly-zone folder sizes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unix seconds when taken.
    pub taken: u64,
    /// Folder path (as string) → size in bytes.
    pub sizes: BTreeMap<String, u64>,
}

/// One folder that grew notably (or newly appeared large) since the prior snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct Growth {
    pub path: PathBuf,
    pub name: String,
    pub kind: AnomalyKind,
    pub current: u64,
    pub previous: Option<u64>,
    /// Absolute growth (or the full size, if new).
    pub delta: u64,
    /// Percent growth vs the prior size (None if new).
    pub pct: Option<f64>,
    /// Days between the prior snapshot and this one.
    pub days: f64,
    pub is_new: bool,
}

/// Result of a `growth` run.
#[derive(Debug, Clone, Serialize)]
pub struct GrowthReport {
    /// Timestamp of the prior snapshot compared against (None on the first run).
    pub previous_at: Option<u64>,
    pub now: u64,
    pub entries: Vec<Growth>,
    /// True when there was no prior snapshot — a baseline was recorded only.
    pub baseline_only: bool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Snapshot the size of every child folder across the anomaly zones.
pub fn snapshot_now() -> Snapshot {
    let mut sizes = BTreeMap::new();
    for zone in default_zones() {
        for container in expand_paths(zone.path) {
            for (path, bytes) in sized_children(&container) {
                sizes.insert(path.to_string_lossy().into_owned(), bytes);
            }
        }
    }
    Snapshot { taken: now_secs(), sizes }
}

fn history_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".deepscan").join("history.json"))
}

/// Load the rolling snapshot history; empty on missing/corrupt.
pub fn load_history() -> Vec<Snapshot> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Persist the history (best-effort; creates `~/.deepscan/`).
pub fn save_history(history: &[Snapshot]) {
    let Some(path) = history_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(history) {
        let _ = std::fs::write(&path, text);
    }
}

/// Compare `current` against `prev`, returning growth entries ranked by delta.
/// Flags a grown folder when it added ≥ 500 MB AND ≥ 25%; a new folder when it
/// is ≥ 500 MB. Pure — no IO.
pub fn compute_growth(prev: &Snapshot, current: &Snapshot) -> Vec<Growth> {
    let days = (current.taken.saturating_sub(prev.taken) as f64) / 86_400.0;
    let mut out = Vec::new();
    for (path_str, &cur) in &current.sizes {
        let previous = prev.sizes.get(path_str).copied();
        let (delta, pct, is_new) = match previous {
            Some(p) => {
                if cur <= p {
                    continue;
                }
                let delta = cur - p;
                let ratio = if p > 0 { delta as f64 / p as f64 } else { f64::INFINITY };
                if delta < GROWTH_FLOOR || ratio < GROWTH_PCT {
                    continue;
                }
                (delta, Some(ratio * 100.0), false)
            }
            None => {
                if cur < GROWTH_FLOOR {
                    continue;
                }
                (cur, None, true)
            }
        };
        let path = PathBuf::from(path_str);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path_str.clone());
        out.push(Growth {
            kind: classify(&path),
            path,
            name,
            current: cur,
            previous,
            delta,
            pct,
            days,
            is_new,
        });
    }
    out.sort_by_key(|g| std::cmp::Reverse(g.delta));
    out
}

/// Take a snapshot, compare to the most recent prior one, append + prune + save.
pub fn record_and_compare() -> GrowthReport {
    let mut history = load_history();
    let current = snapshot_now();
    let now = current.taken;

    let report = match history.last() {
        Some(prev) => GrowthReport {
            previous_at: Some(prev.taken),
            now,
            entries: compute_growth(prev, &current),
            baseline_only: false,
        },
        None => GrowthReport {
            previous_at: None,
            now,
            entries: Vec::new(),
            baseline_only: true,
        },
    };

    history.push(current);
    let excess = history.len().saturating_sub(MAX_HISTORY);
    if excess > 0 {
        history.drain(0..excess);
    }
    save_history(&history);

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(taken: u64, entries: &[(&str, u64)]) -> Snapshot {
        Snapshot {
            taken,
            sizes: entries.iter().map(|(p, b)| (p.to_string(), *b)).collect(),
        }
    }

    const GB: u64 = 1024 * MB;

    #[test]
    fn flags_big_fast_growth_not_small_ticks() {
        let day = 86_400;
        let prev = snap(
            0,
            &[
                ("/Users/x/Library/Application Support/Claude", 2 * GB),
                ("/Users/x/Library/Caches/steady", 10 * GB),
            ],
        );
        let current = snap(
            3 * day,
            &[
                ("/Users/x/Library/Application Support/Claude", 20 * GB), // +18 GB, +900%
                ("/Users/x/Library/Caches/steady", 10 * GB + 100 * MB),   // +100 MB, +1% → below floor+pct
            ],
        );
        let g = compute_growth(&prev, &current);
        assert_eq!(g.len(), 1, "only the big + fast grower is flagged");
        assert_eq!(g[0].name, "Claude");
        assert_eq!(g[0].kind, AnomalyKind::AppData);
        assert!(!g[0].is_new);
        assert!((g[0].days - 3.0).abs() < 0.001);
    }

    #[test]
    fn flags_new_large_folder() {
        let prev = snap(0, &[("/Users/x/Library/Caches/old", MB)]);
        let current = snap(
            86_400,
            &[
                ("/Users/x/Library/Caches/old", MB),
                ("/Users/x/Library/Application Support/Leak", 3 * GB), // new, 3 GB
            ],
        );
        let g = compute_growth(&prev, &current);
        assert_eq!(g.len(), 1);
        assert!(g[0].is_new);
        assert_eq!(g[0].name, "Leak");
        assert_eq!(g[0].previous, None);
    }

    #[test]
    fn small_new_folder_not_flagged() {
        let prev = snap(0, &[]);
        let current = snap(86_400, &[("/Users/x/Library/Caches/tiny", 10 * MB)]);
        assert!(compute_growth(&prev, &current).is_empty());
    }

    #[test]
    fn snapshot_json_round_trips() {
        // Round-trip through serde against a TEMP file — never touch ~/.deepscan.
        let base = std::env::temp_dir().join(format!("deepscan-hist-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("history.json");
        let history = vec![snap(1, &[("/a", 10)]), snap(2, &[("/a", 20)])];
        std::fs::write(&file, serde_json::to_string(&history).unwrap()).unwrap();
        let loaded: Vec<Snapshot> =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].sizes.get("/a"), Some(&20));
        let _ = std::fs::remove_dir_all(&base);
    }
}
```

- [ ] **Step 4: Register the module + re-exports**

In `crates/deepscan-core/src/lib.rs`, add `pub mod baseline;` with the other `pub mod` lines, and add a re-export:
```rust
pub use baseline::{
    compute_growth, load_history, record_and_compare, save_history, snapshot_now, Growth,
    GrowthReport, Snapshot,
};
```

- [ ] **Step 5: Run the tests + gate**

Run: `cargo test -p deepscan-core baseline` (expect 4 pass), then `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test -q`.
Expected: all pass, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(core): temporal self-baseline — snapshot history + growth detection"
```

---

### Task 2: CLI `growth` command

**Files:**
- Modify: `crates/deepscan-cli/src/main.rs` (`Growth` command variant, match arm, `run_growth`, `render_growth`, `human_duration`, import `record_and_compare`)

**Interfaces:**
- Consumes: `deepscan_core::{record_and_compare, GrowthReport, AnomalyKind}` (AnomalyKind already imported), `with_spinner`, `palette`, `human`.

- [ ] **Step 1: Add the `Growth` command + import**

In `main.rs`, add `record_and_compare` to the `use deepscan_core::{…}` block. Add to the `Commands` enum (after `Explore`):
```rust
    /// Record a size snapshot and report folders growing abnormally fast
    /// (temporal baseline; history in ~/.deepscan/history.json).
    Growth {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
```
Add the match arm in `main`:
```rust
        Commands::Growth { json } => run_growth(json),
```

- [ ] **Step 2: Implement `run_growth` + `render_growth` + `human_duration`**

Add to `main.rs`:
```rust
fn run_growth(json: bool) -> anyhow::Result<ExitCode> {
    let report = with_spinner("scanning zones", record_and_compare);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(ExitCode::SUCCESS);
    }
    render_growth(&report);
    Ok(ExitCode::SUCCESS)
}

fn render_growth(report: &deepscan_core::GrowthReport) {
    let Palette {
        bold,
        dim,
        yellow,
        cyan,
        reset,
        ..
    } = palette();

    if report.baseline_only {
        println!(
            "{bold}deepscan growth{reset} {dim}· baseline recorded — run again later to see what grew{reset}"
        );
        return;
    }

    let since = human_duration(report.now.saturating_sub(report.previous_at.unwrap_or(report.now)));
    println!("{bold}deepscan growth{reset} {dim}· since {since} ago{reset}");
    if report.entries.is_empty() {
        println!("  {dim}nothing grew notably since the last snapshot{reset}");
        return;
    }
    for g in &report.entries {
        let (tag, color) = match g.kind {
            AnomalyKind::AppData => ("app data", yellow),
            AnomalyKind::Cache => ("cache", dim),
        };
        let change = if g.is_new {
            format!("NEW {}", human(g.current))
        } else {
            let pct = g.pct.map(|p| format!(" (+{p:.0}%)")).unwrap_or_default();
            format!("+{}{}", human(g.delta), pct)
        };
        let rate = if g.days >= 1.0 && !g.is_new {
            format!(" {dim}· {}/day{reset}", human((g.delta as f64 / g.days) as u64))
        } else {
            String::new()
        };
        println!(
            "  {color}{:>18}{reset}  {cyan}{}{reset} {dim}[{tag}]{reset}{}",
            change, g.name, rate
        );
        println!("      {dim}{}{reset}", g.path.display());
    }
}

/// Coarse "3d" / "5h" / "12m" duration for the "since … ago" header.
fn human_duration(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}
```

- [ ] **Step 3: Build + smoke-test (records + reports)**

```bash
cd /Users/dmitrijacenko/projects/deepscan
cargo build -q --release -p deepscan-cli
# First run records a baseline (no history yet):
rm -f ~/.deepscan/history.json
./target/release/deepscan growth 2>/dev/null | head -2      # "baseline recorded …"
# Second run compares (nothing changed in seconds → likely "nothing grew"):
./target/release/deepscan growth 2>/dev/null | head -2
# JSON valid:
./target/release/deepscan growth --json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print('growth json ok · baseline_only=',d['baseline_only'])"
# history file exists and is valid JSON:
python3 -c "import json;json.load(open('$HOME/.deepscan/history.json'));print('history.json valid')"
```
Expected: first run "baseline recorded"; second run runs cleanly; JSON valid; history.json valid. (No folder will have grown 500 MB in seconds, so the list is empty — that's correct.)

- [ ] **Step 4: clippy + full test + commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test -q
git add -A && git commit -m "feat(cli): deepscan growth — report abnormally fast-growing folders"
```

---

## Exit criteria

- `deepscan growth` records a size snapshot to `~/.deepscan/history.json` and, from the second run on, reports folders that grew ≥ 500 MB & ≥ 25% (or newly appeared ≥ 500 MB) since the prior snapshot — ranked by growth, tagged cache vs app-data, with % and per-day rate.
- History is rolling (last 30), tolerant of missing/corrupt; `--json` emits the report; no other command's behavior or speed changes.
- `compute_growth` is unit-tested; CI green.

Deferred: interactive/TUI growth view; cross-machine community baseline (needs a backend); recording snapshots automatically on other commands.
