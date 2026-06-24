//! The differentiated layer — leak/anomaly signatures.
//!
//! Signatures are *data, not code*: the default set is embedded from the
//! repo-root `signatures.toml`, and anyone can ship their own with
//! `--signatures`. A signature fires only when its (glob-expanded) path
//! exceeds `baseline_mb`, so it reports a *leak*, not just a big folder.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::report::{Finding, Severity};
use crate::scan::{count_matching_files, measure_path};

#[derive(Debug, Deserialize)]
struct SignaturesFile {
    #[serde(default, rename = "signature")]
    signatures: Vec<Signature>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Signature {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// May contain `~`, `$VARS`, and glob wildcards.
    pub path: String,
    #[serde(default)]
    pub file_pattern: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    pub baseline_mb: u64,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub root_cause: Option<String>,
    #[serde(default)]
    pub prevention: Option<String>,
    #[serde(default)]
    pub safe_delete: Option<String>,
}

/// The signature set shipped in the binary.
pub fn default_signatures() -> Vec<Signature> {
    parse_signatures(include_str!("../../../signatures.toml")).unwrap_or_default()
}

pub fn parse_signatures(raw: &str) -> anyhow::Result<Vec<Signature>> {
    let file: SignaturesFile = toml::from_str(raw)?;
    Ok(file.signatures)
}

pub fn load_signatures(path: &Path) -> anyhow::Result<Vec<Signature>> {
    let raw = std::fs::read_to_string(path)?;
    parse_signatures(&raw)
}

/// Expand `~`/`$VARS` then glob; falls back to a literal path check.
pub(crate) fn expand_paths(pattern: &str) -> Vec<PathBuf> {
    let expanded = shellexpand::full(pattern)
        .map(|cow| cow.into_owned())
        .unwrap_or_else(|_| pattern.to_string());

    match glob::glob(&expanded) {
        Ok(paths) => paths.filter_map(Result::ok).collect(),
        Err(_) => {
            let path = PathBuf::from(&expanded);
            if path.exists() {
                vec![path]
            } else {
                Vec::new()
            }
        }
    }
}

fn severity_for(signature: &Signature, ratio: f64) -> Severity {
    if let Some(level) = &signature.severity {
        match level.to_ascii_lowercase().as_str() {
            "critical" | "crit" => return Severity::Critical,
            "warn" | "warning" => return Severity::Warn,
            "info" => return Severity::Info,
            _ => {}
        }
    }
    if ratio >= 10.0 {
        Severity::Critical
    } else if ratio >= 2.0 {
        Severity::Warn
    } else {
        Severity::Info
    }
}

/// Evaluate every signature, returning fired findings sorted largest first.
pub fn evaluate_signatures(signatures: &[Signature]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for signature in signatures {
        let baseline_bytes = signature.baseline_mb.saturating_mul(1024 * 1024);
        let pattern = signature
            .file_pattern
            .as_ref()
            .and_then(|raw| glob::Pattern::new(raw).ok());

        for dir in expand_paths(&signature.path) {
            let bytes = measure_path(&dir);
            if bytes <= baseline_bytes {
                continue;
            }

            let ratio = bytes as f64 / baseline_bytes.max(1) as f64;
            let safe_delete = signature
                .safe_delete
                .clone()
                .unwrap_or_else(|| format!("rm -rf \"{}\"", dir.display()));

            findings.push(Finding {
                name: signature.name.clone(),
                description: signature.description.clone(),
                file_matches: pattern.as_ref().map(|pat| count_matching_files(&dir, pat)),
                path: dir,
                bytes,
                baseline_bytes,
                owner: signature.owner.clone(),
                severity: severity_for(signature, ratio),
                root_cause: signature.root_cause.clone(),
                prevention: signature.prevention.clone(),
                safe_delete,
            });
        }
    }

    findings.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    findings
}
