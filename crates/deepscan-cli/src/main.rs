//! deepscan — fast macOS disk forensics.
//!
//! `deepscan scan [PATH]` prints three sections:
//!   1. where the space is (top children of the root)
//!   2. reclaimable buckets (broad, every-Mac coverage)
//!   3. leak signatures (anomalies above baseline, with root cause + fix)

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use deepscan_core::{
    default_catalog, default_signatures, evaluate_catalog, evaluate_signatures, human,
    load_signatures, scan_children, Severity,
};

#[derive(Parser)]
#[command(
    name = "deepscan",
    version,
    about = "Fast macOS disk forensics — broad coverage + leak signatures"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan a directory tree: reclaimable space + leak signatures.
    Scan {
        /// Root to scan (default: your home directory).
        path: Option<PathBuf>,
        /// Show the top N largest children.
        #[arg(long, default_value_t = 12)]
        top: usize,
        /// Use a custom signatures file instead of the built-in set.
        #[arg(long)]
        signatures: Option<PathBuf>,
        /// Skip the broad child-size breakdown.
        #[arg(long)]
        no_tree: bool,
    },
}

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Scan {
            path,
            top,
            signatures,
            no_tree,
        } => {
            let root = path.unwrap_or_else(home);
            run_scan(&root, top, signatures.as_deref(), no_tree)
        }
    }
}

fn run_scan(
    root: &Path,
    top: usize,
    signatures_path: Option<&Path>,
    no_tree: bool,
) -> anyhow::Result<()> {
    println!("{BOLD}deepscan{RESET} {DIM}· scanning {}{RESET}", root.display());

    if !no_tree {
        let (total, children) = scan_children(root)?;
        println!(
            "\n{BOLD}Where the space is{RESET} {DIM}(total {}){RESET}",
            human(total)
        );
        for child in children.iter().take(top).filter(|c| c.bytes > 0) {
            println!("  {:>11}  {}", human(child.bytes), child.name);
        }
    }

    let buckets = evaluate_catalog(&default_catalog());
    if !buckets.is_empty() {
        let reclaimable: u64 = buckets.iter().map(|b| b.bytes).sum();
        println!(
            "\n{BOLD}Reclaimable buckets{RESET} {DIM}({} across {} locations){RESET}",
            human(reclaimable),
            buckets.len()
        );
        for bucket in &buckets {
            println!(
                "  {:>11}  {CYAN}{}{RESET} {DIM}— {}{RESET}",
                human(bucket.bytes),
                bucket.name,
                bucket.note
            );
        }
    }

    let signatures = match signatures_path {
        Some(path) => load_signatures(path)?,
        None => default_signatures(),
    };
    let findings = evaluate_signatures(&signatures);

    println!("\n{BOLD}Leak signatures{RESET}");
    if findings.is_empty() {
        println!("  {DIM}no anomalies above baseline — clean{RESET}");
    } else {
        for finding in &findings {
            let (color, tag) = match finding.severity {
                Severity::Critical => (RED, "CRIT"),
                Severity::Warn => (YELLOW, "WARN"),
                Severity::Info => (DIM, "INFO"),
            };
            println!(
                "  {color}\u{26a0} [{tag}] {}{RESET} {BOLD}{}{RESET} {DIM}(baseline {}){RESET}",
                finding.name,
                human(finding.bytes),
                human(finding.baseline_bytes)
            );
            if let Some(owner) = &finding.owner {
                println!("      owner:    {owner}");
            }
            println!("      path:     {}", finding.path.display());
            if let Some(matches) = finding.file_matches {
                println!("      matches:  {matches} files");
            }
            if let Some(cause) = &finding.root_cause {
                println!("      cause:    {cause}");
            }
            if let Some(prevention) = &finding.prevention {
                println!("      prevent:  {prevention}");
            }
            println!("      reclaim:  {}", finding.safe_delete);
        }
    }

    Ok(())
}
