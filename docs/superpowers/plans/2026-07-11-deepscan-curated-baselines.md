# deepscan curated baselines — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Judge a folder's size against **what's typical for that app**, not just "big" — via a community-curated `baselines.toml` (contributed by PR, never telemetry). `anomalies` annotates matches ("typical ~800 MB · 25.6× above typical"), and `deepscan baseline --suggest` prints `[[baseline]]` rows measured from your own machine so contributing is copy-paste.

**Architecture:** Mirrors the existing `signatures.toml` pattern exactly — a repo-root TOML embedded with `include_str!`, `default_baselines()`/`parse_baselines()`/`load_baselines()`, data-driven and community-extensible. New `deepscan-core/src/baselines.rs`; CLI gains a `baseline` command and an additive annotation in `anomalies`.

**Tech Stack:** Rust 2021, serde + toml (already core deps), serde_json's `json!` macro in the CLI (no new deps).

## Global Constraints

- Toolchain 1.96.0; every task ends fmt-clean, `cargo clippy --all-targets -- -D warnings` clean, all tests passing.
- **Never invent baseline numbers.** `baselines.toml` ships with zero entries by design; entries come only from real measurements (`--suggest`). The plan must not add fabricated data.
- The `anomalies` annotation is **additive**: with an empty `baselines.toml` nothing matches, so existing `anomalies` output (human + `--json`) is byte-identical to today.
- Zone strings must match `anomaly::default_zones()` names exactly: `Per-user temp`, `Per-user temp caches`, `User caches`, `App containers`, `Application Support`, `Group containers`.
- No telemetry, no network calls, ever.

---

### Task 1: Core `baselines.rs` + the `baselines.toml` data file

**Files:**
- Create: `crates/deepscan-core/src/baselines.rs`
- Modify: `crates/deepscan-core/src/lib.rs` (`pub mod baselines;` + re-exports)
- (`baselines.toml` already exists at the repo root — do not add entries to it.)

**Interfaces:**
- Produces: `Baseline { name: String, zone: String, typical_mb: u64, observations: u32, note: Option<String> }`
- Produces: `default_baselines() -> Vec<Baseline>`, `parse_baselines(&str) -> anyhow::Result<Vec<Baseline>>`, `load_baselines(&Path) -> anyhow::Result<Vec<Baseline>>`
- Produces: `lookup_baseline<'a>(&'a [Baseline], name: &str, zone: &str) -> Option<&'a Baseline>`, `baseline_ratio(bytes: u64, &Baseline) -> Option<f64>`, `is_above_typical(bytes: u64, &Baseline) -> bool`

- [ ] **Step 1: Write `baselines.rs` with the parser, lookup, and tests**

Create `crates/deepscan-core/src/baselines.rs`:

```rust
//! Curated per-app size baselines — the cross-machine baseline done over **git
//! instead of telemetry**. `anomalies` shows the biggest same-class folders;
//! a baseline answers whether that size is *normal for that app*.
//!
//! Data lives in the repo-root `baselines.toml` (same data-driven,
//! community-extensible shape as `signatures.toml`): contributions arrive as
//! pull requests, so deepscan never phones home and never uploads a map of
//! your filesystem. Entries must be real measurements — `deepscan baseline
//! --suggest` prints rows from the running machine. A baseline is only as good
//! as its sample, so `observations` records how many machines it came from.

use std::path::Path;

use serde::Deserialize;

const MB: u64 = 1024 * 1024;
/// Call a folder out once it reaches this multiple of its typical size.
const ABOVE_FACTOR: f64 = 3.0;

/// A typical size for one app's folder in one zone.
#[derive(Debug, Clone, Deserialize)]
pub struct Baseline {
    /// Folder name to match (case-insensitive).
    pub name: String,
    /// Zone name, matching `anomaly::default_zones()` (e.g. "Application Support").
    pub zone: String,
    /// Typical observed size, in MB.
    pub typical_mb: u64,
    /// How many machines contributed this number.
    #[serde(default = "one")]
    pub observations: u32,
    #[serde(default)]
    pub note: Option<String>,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
struct BaselineFile {
    #[serde(default)]
    baseline: Vec<Baseline>,
}

/// The built-in curated set (the embedded `baselines.toml`).
pub fn default_baselines() -> Vec<Baseline> {
    parse_baselines(include_str!("../../../baselines.toml")).unwrap_or_default()
}

pub fn parse_baselines(raw: &str) -> anyhow::Result<Vec<Baseline>> {
    let file: BaselineFile = toml::from_str(raw)?;
    Ok(file.baseline)
}

pub fn load_baselines(path: &Path) -> anyhow::Result<Vec<Baseline>> {
    let raw = std::fs::read_to_string(path)?;
    parse_baselines(&raw)
}

/// The baseline for a folder `name` in `zone`, if one has been contributed.
pub fn lookup_baseline<'a>(
    baselines: &'a [Baseline],
    name: &str,
    zone: &str,
) -> Option<&'a Baseline> {
    baselines
        .iter()
        .find(|b| b.zone == zone && b.name.eq_ignore_ascii_case(name))
}

/// How many times its typical size this folder is. `None` when the baseline
/// says 0 MB (no meaningful ratio).
pub fn baseline_ratio(bytes: u64, baseline: &Baseline) -> Option<f64> {
    let typical = baseline.typical_mb.saturating_mul(MB);
    if typical == 0 {
        return None;
    }
    Some(bytes as f64 / typical as f64)
}

/// True when the folder is far enough above typical to be worth calling out.
pub fn is_above_typical(bytes: u64, baseline: &Baseline) -> bool {
    baseline_ratio(bytes, baseline).is_some_and(|r| r >= ABOVE_FACTOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[baseline]]
name = "Claude"
zone = "Application Support"
typical_mb = 800
note = "Electron app data"

[[baseline]]
name = "com.spotify.client"
zone = "User caches"
typical_mb = 900
observations = 4
"#;

    #[test]
    fn parses_and_defaults_observations() {
        let b = parse_baselines(SAMPLE).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].name, "Claude");
        assert_eq!(b[0].typical_mb, 800);
        assert_eq!(b[0].observations, 1, "defaults to a single machine");
        assert_eq!(b[1].observations, 4);
    }

    #[test]
    fn lookup_is_case_insensitive_and_zone_scoped() {
        let b = parse_baselines(SAMPLE).unwrap();
        assert!(lookup_baseline(&b, "claude", "Application Support").is_some());
        assert!(
            lookup_baseline(&b, "Claude", "User caches").is_none(),
            "zone must match too"
        );
        assert!(lookup_baseline(&b, "Unknown", "Application Support").is_none());
    }

    #[test]
    fn flags_only_well_above_typical() {
        let b = parse_baselines(SAMPLE).unwrap();
        let claude = lookup_baseline(&b, "Claude", "Application Support").unwrap();
        // 20 GB against an 800 MB typical → 25.6×
        let ratio = baseline_ratio(20 * 1024 * MB, claude).unwrap();
        assert!((ratio - 25.6).abs() < 0.1);
        assert!(is_above_typical(20 * 1024 * MB, claude));
        // 1 GB against 800 MB → 1.28×: big-ish, but normal for this app.
        assert!(!is_above_typical(1024 * MB, claude));
    }

    #[test]
    fn shipped_baselines_file_parses() {
        // Regression net for the shipped data file (it is empty today, and an
        // empty/comment-only file must still parse to zero baselines).
        let shipped = parse_baselines(include_str!("../../../baselines.toml"))
            .expect("shipped baselines.toml must parse");
        assert!(shipped.iter().all(|b| b.typical_mb > 0));
    }
}
```

- [ ] **Step 2: Register the module + re-exports**

In `crates/deepscan-core/src/lib.rs`, add `pub mod baselines;` with the other `pub mod` lines, and:
```rust
pub use baselines::{
    baseline_ratio, default_baselines, is_above_typical, load_baselines, lookup_baseline,
    parse_baselines, Baseline,
};
```

- [ ] **Step 3: Run the tests + gate**

Run: `cargo test -p deepscan-core baselines` (expect 4 pass), then `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test -q`.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(core): curated per-app size baselines (baselines.toml, git not telemetry)"
```

---

### Task 2: CLI — annotate `anomalies` + the `baseline` command

**Files:**
- Modify: `crates/deepscan-cli/src/main.rs` (imports; `Baseline` command variant + arm; annotate `run_anomalies`; add `run_baseline`)

**Interfaces:**
- Consumes: `deepscan_core::{default_baselines, lookup_baseline, baseline_ratio, is_above_typical, detect_anomalies, default_zones}`.

- [ ] **Step 1: Imports + the `baseline` command variant**

Add to the `use deepscan_core::{…}` block: `baseline_ratio, default_baselines, is_above_typical, lookup_baseline`.

Add to the `Commands` enum (after `Growth`):
```rust
    /// Compare your folder sizes against the curated per-app baselines, or
    /// print [[baseline]] rows measured here to contribute (baselines.toml).
    Baseline {
        /// Print [[baseline]] TOML rows for folders that have no baseline yet,
        /// measured on this machine — review, then PR them into baselines.toml.
        #[arg(long)]
        suggest: bool,
        /// Emit machine-readable JSON (the comparison view).
        #[arg(long, conflicts_with = "suggest")]
        json: bool,
    },
```
Add the match arm in `main`:
```rust
        Commands::Baseline { suggest, json } => run_baseline(suggest, json),
```

- [ ] **Step 2: Annotate `anomalies` rows when a baseline exists (additive)**

In `run_anomalies`, after the `anomalies` are computed and before rendering, add:
```rust
    let baselines = default_baselines();
```
Then define a small helper near `run_anomalies`:
```rust
/// " · typical ~800 MB · 25.6× above typical" when a curated baseline exists
/// for this folder; empty string otherwise (so output is unchanged until
/// baselines.toml has entries).
fn baseline_note(
    baselines: &[deepscan_core::Baseline],
    name: &str,
    zone: &str,
    bytes: u64,
) -> String {
    let Palette {
        dim, red, reset, ..
    } = palette();
    let Some(b) = lookup_baseline(baselines, name, zone) else {
        return String::new();
    };
    let typical = human(b.typical_mb.saturating_mul(1024 * 1024));
    match baseline_ratio(bytes, b) {
        Some(r) if is_above_typical(bytes, b) => {
            format!(" {dim}· typical ~{typical}{reset} {red}· {r:.1}× above typical{reset}")
        }
        _ => format!(" {dim}· typical ~{typical}{reset}"),
    }
}
```
Then append it to BOTH row prints in `run_anomalies` — the app-data row and the cache row — e.g. the app-data print becomes:
```rust
            println!(
                "  {color}[{tag}]{reset} {bold}{:>9}{reset}  {}  {dim}in {}{reset}{}",
                human(anomaly.bytes),
                anomaly.name,
                anomaly.zone,
                baseline_note(&baselines, &anomaly.name, &anomaly.zone, anomaly.bytes)
            );
```
and the cache print similarly gains the trailing `{}` + the same `baseline_note(...)` argument. (With the shipped empty `baselines.toml` this always appends `""`, so today's output is unchanged.)

- [ ] **Step 3: Implement `run_baseline`**

```rust
fn run_baseline(suggest: bool, json: bool) -> anyhow::Result<ExitCode> {
    let baselines = default_baselines();
    let anomalies = with_spinner("measuring zones", || detect_anomalies(&default_zones()));

    let Palette {
        bold,
        dim,
        red,
        green,
        cyan,
        reset,
        ..
    } = palette();

    if suggest {
        let missing: Vec<_> = anomalies
            .iter()
            .filter(|a| lookup_baseline(&baselines, &a.name, &a.zone).is_none())
            .collect();
        println!(
            "{dim}# measured on this machine — review, then PR these into baselines.toml{reset}"
        );
        if missing.is_empty() {
            println!("{dim}# every notable folder here already has a baseline{reset}");
            return Ok(ExitCode::SUCCESS);
        }
        for a in missing {
            println!();
            println!("[[baseline]]");
            println!("name = \"{}\"", a.name);
            println!("zone = \"{}\"", a.zone);
            println!("typical_mb = {}", a.bytes / (1024 * 1024));
            println!("observations = 1");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let matched: Vec<_> = anomalies
        .iter()
        .filter_map(|a| lookup_baseline(&baselines, &a.name, &a.zone).map(|b| (a, b)))
        .collect();

    if json {
        let payload: Vec<_> = matched
            .iter()
            .map(|(a, b)| {
                serde_json::json!({
                    "name": a.name,
                    "zone": a.zone,
                    "bytes": a.bytes,
                    "typical_mb": b.typical_mb,
                    "observations": b.observations,
                    "ratio": baseline_ratio(a.bytes, b),
                    "above_typical": is_above_typical(a.bytes, b),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "{bold}deepscan baseline{reset} {dim}· your sizes vs curated typical ({} baseline(s)){reset}",
        baselines.len()
    );
    if baselines.is_empty() {
        println!(
            "  {dim}baselines.toml is empty — run `deepscan baseline --suggest` and open a PR to seed it.{reset}"
        );
        println!(
            "  {dim}(baselines are curated over git, never telemetry — deepscan does not phone home){reset}"
        );
        return Ok(ExitCode::SUCCESS);
    }
    if matched.is_empty() {
        println!("  {dim}none of your notable folders have a baseline yet{reset}");
        return Ok(ExitCode::SUCCESS);
    }
    for (a, b) in matched {
        let verdict = match baseline_ratio(a.bytes, b) {
            Some(r) if is_above_typical(a.bytes, b) => {
                format!("{red}{r:.1}× above typical{reset}")
            }
            Some(r) => format!("{green}normal ({r:.1}×){reset}"),
            None => format!("{dim}—{reset}"),
        };
        println!(
            "  {:>10}  {cyan}{}{reset} {dim}[{}] · typical ~{} · {} obs{reset} · {}",
            human(a.bytes),
            a.name,
            a.zone,
            human(b.typical_mb.saturating_mul(1024 * 1024)),
            b.observations,
            verdict
        );
    }
    Ok(ExitCode::SUCCESS)
}
```

- [ ] **Step 4: Build + smoke-test**

```bash
cd /Users/dmitrijacenko/projects/deepscan
cargo build -q --release -p deepscan-cli
# anomalies output UNCHANGED (baselines.toml ships empty → nothing annotated):
timeout 120 ./target/release/deepscan anomalies 2>/dev/null | head -4
# baseline default view explains the empty state:
timeout 120 ./target/release/deepscan baseline 2>/dev/null | head -3
# --suggest prints real TOML rows measured here:
timeout 120 ./target/release/deepscan baseline --suggest 2>/dev/null | head -8
# --json valid (empty array while baselines.toml is empty):
timeout 120 ./target/release/deepscan baseline --json 2>/dev/null | python3 -c "import json,sys;print('baseline json ok ·',len(json.load(sys.stdin)),'matches')"
# the suggested rows must actually parse as baselines:
timeout 120 ./target/release/deepscan baseline --suggest 2>/dev/null | grep -v '^#' > /tmp/ds-suggest.toml && python3 -c "print('suggest rows written')"
```
Expected: `anomalies` unchanged; `baseline` says the file is empty + how to seed; `--suggest` prints `[[baseline]]` blocks with real measured `typical_mb`; `--json` is a valid (empty) array.

- [ ] **Step 5: clippy + full test + commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test -q
git add -A && git commit -m "feat(cli): deepscan baseline — compare vs curated typical + --suggest rows to PR"
```

---

## Exit criteria

- `baselines.toml` (repo root, embedded) is a documented, community-curated data file shipping **zero invented entries**; it parses (regression-tested).
- `deepscan baseline` compares your folders to curated typicals; `--suggest` prints real `[[baseline]]` rows measured here, ready to PR; `--json` emits the comparison.
- `anomalies` annotates rows with "typical ~X · N× above typical" when a baseline exists, and is byte-identical to today while the file is empty.
- No telemetry, no network. fmt/clippy/tests clean; CI green.

Deferred: annotating the `scan` dashboard + `growth` with baselines; a `--baselines <path>` override flag (the loader exists); the real cross-machine telemetry baseline (blocked on having a user base).
