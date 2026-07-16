# deepscan `growth` — temporal self-baseline (design)

**Date:** 2026-07-11
**Status:** Approved design, pre-implementation

## Problem

`anomalies` and the `scan` dashboard classify the *biggest same-class folders* honestly (cache vs app-data), but that's still not **anomaly detection** — a 20 GB folder might be perfectly normal for that app. The only thing that turns "big" into "abnormal" is a comparison. This feature adds the local half of that: **compare each folder to its own past**, so we can flag folders that grew *abnormally fast* — the real signal (and the shape of the original idleassetsd leak: a folder that ballooned).

## Approach

A new `deepscan growth` command that, each run:
1. Takes a **snapshot** of the sizes of every child folder across the anomaly zones (`~/Library/Caches`, `~/Library/Application Support`, Containers, Group Containers, per-user temp).
2. Loads the rolling **history** from `~/.deepscan/history.json`, compares the current snapshot to the most recent prior one, and reports folders growing fastest.
3. Appends the current snapshot to history (capped at the last 30, oldest pruned) and saves.

So running `deepscan growth` occasionally builds the baseline; the first run records and says "run again later."

## What "abnormal growth" means

For each folder in the current snapshot, compared to the most recent prior snapshot:
- **Grown**: present before, `delta = current − previous > 0`, flagged when `delta ≥ 500 MB` **and** `delta / previous ≥ 25%`. (Both gates so a steady large cache ticking up isn't flagged, but a real balloon is.)
- **New**: absent from the prior snapshot and `current ≥ 500 MB` — the "appeared and ballooned" leak shape.
- Each entry is tagged **cache** (regenerable, expected to grow — informational) vs **app data** (the real signal) via the existing Phase-1 classifier, so a churning build cache reads as low-priority.
- Entries ranked by absolute `delta`, largest first. Output shows delta, %, time since the prior snapshot, and a per-day rate when the interval is ≥ 1 day.

## Storage

- `~/.deepscan/history.json` — a JSON array of snapshots `{ taken: <unix secs>, sizes: { "<path>": <bytes>, … } }`, newest last, capped at 30. Tolerant of a missing/corrupt file (starts fresh). Created on first run.
- The set of folders snapshotted = every child dir of the anomaly zones (a few hundred entries, bounded).

## Surface

- `deepscan growth [--json] [--plain]` — a printed report (TTY-colored via the existing palette; plain when piped). Not a TUI (growth is investigative — you look, you don't select→Trash), so no interactive mode for now.
- Standalone: does **not** touch `scan`'s fast default path or any other command; recording happens only when `growth` runs.

## Scope / non-goals

- No interactive/TUI view (deferred; growth has no Trash action).
- No cross-machine/community baseline (separate feature, needs a backend).
- Snapshotting is opt-in via running `growth` — no background daemon, no silent recording on other commands.
- History is size-only (path → bytes); no per-file detail.

## Module layout

- `deepscan-core/src/baseline.rs` — `Snapshot`, `Growth`, `GrowthReport`, `compute_growth` (pure), and the IO (`snapshot_now`, `load_history`/`save_history`, `record_and_compare`). Reuses `anomaly::{default_zones, sized_children, classify}` (exposed `pub(crate)`) + `scan::measure_path`.
- `deepscan-cli/src/main.rs` — `Growth` clap command + `run_growth` + a render fn.
