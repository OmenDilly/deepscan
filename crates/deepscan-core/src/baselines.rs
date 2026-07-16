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
//! Below [`MIN_OBSERVATIONS`] an entry accumulates but never flags anyone —
//! one machine's size is an observation, not a typical.

use std::path::Path;

use serde::Deserialize;

const MB: u64 = 1024 * 1024;
/// Call a folder out once it reaches this multiple of its typical size.
const ABOVE_FACTOR: f64 = 3.0;
/// A baseline needs this many independent machines before it's trusted to flag
/// anyone. Below it an entry just accumulates — contributing a single
/// observation is safe: it can't misjudge someone else's machine.
pub const MIN_OBSERVATIONS: u32 = 3;

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

/// True once enough machines agree for this baseline to judge others.
pub fn is_usable(baseline: &Baseline) -> bool {
    baseline.observations >= MIN_OBSERVATIONS
}

/// True when the folder is far enough above typical to be worth calling out.
/// Never fires for a baseline with too few observations — one machine's size
/// is an observation, not a typical.
pub fn is_above_typical(bytes: u64, baseline: &Baseline) -> bool {
    is_usable(baseline) && baseline_ratio(bytes, baseline).is_some_and(|r| r >= ABOVE_FACTOR)
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
        // Ratio math is observation-count-agnostic; use the 4-observation
        // entry so the flag itself (gated on is_usable) can fire too.
        let b = parse_baselines(SAMPLE).unwrap();
        let spotify = lookup_baseline(&b, "com.spotify.client", "User caches").unwrap();
        // 9000 MB against a 900 MB typical → 10×
        let ratio = baseline_ratio(9000 * MB, spotify).unwrap();
        assert!((ratio - 10.0).abs() < 0.01);
        assert!(is_above_typical(9000 * MB, spotify));
        // 1800 MB against 900 MB → 2×: big-ish, but normal for this app.
        assert!(!is_above_typical(1800 * MB, spotify));
    }

    #[test]
    fn single_observation_never_flags() {
        let b = parse_baselines(SAMPLE).unwrap();
        let claude = lookup_baseline(&b, "Claude", "Application Support").unwrap();
        assert_eq!(claude.observations, 1);
        assert!(!is_usable(claude));
        // 20 GB vs an 800 MB "typical" is 25.6×, but one machine can't judge.
        assert!(baseline_ratio(20 * 1024 * MB, claude).unwrap() > 3.0);
        assert!(
            !is_above_typical(20 * 1024 * MB, claude),
            "1 obs must not flag"
        );
        let spotify = lookup_baseline(&b, "com.spotify.client", "User caches").unwrap();
        assert_eq!(spotify.observations, 4);
        assert!(is_usable(spotify));
        assert!(is_above_typical(10 * 1024 * MB, spotify), "4 obs may flag");
    }

    #[test]
    fn shipped_baselines_file_parses() {
        // Regression net for the shipped data file (it is empty today, and an
        // empty/comment-only file must still parse to zero baselines).
        let shipped = parse_baselines(include_str!("../../../baselines.toml"))
            .expect("shipped baselines.toml must parse");
        assert!(shipped.iter().all(|b| b.typical_mb > 0));

        let zones: Vec<&str> = crate::anomaly::default_zones()
            .iter()
            .map(|z| z.name)
            .collect();
        assert!(
            shipped.iter().all(|b| zones.contains(&b.zone.as_str())),
            "every baseline zone must match a default_zones() name"
        );
    }
}
