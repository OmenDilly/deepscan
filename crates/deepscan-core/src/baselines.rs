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
