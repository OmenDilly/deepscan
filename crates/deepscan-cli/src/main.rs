//! deepscan — fast macOS disk forensics.
//!
//!   deepscan scan [PATH] [--json]      where space is + reclaimable + leaks
//!   deepscan reclaim [--apply] [--yes] guarded cleanup of regenerable caches

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use deepscan_core::{
    analyze_container, build_reclaim_plan, build_report, default_signatures, default_zones,
    detect_anomalies, execute_reclaim, home_dir, human, load_signatures, ReclaimPlan, ScanReport,
    Severity, TreeNode,
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
        /// Nesting depth of the size tree (1 = flat top level).
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Use a custom signatures file instead of the built-in set.
        #[arg(long)]
        signatures: Option<PathBuf>,
        /// Skip the broad child-size breakdown.
        #[arg(long)]
        no_tree: bool,
        /// Emit machine-readable JSON instead of the formatted report.
        #[arg(long)]
        json: bool,
    },
    /// Find size outliers vs sibling directories (learned baselines).
    Anomalies {
        /// Analyze one directory's children instead of the default zones.
        path: Option<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Reclaim regenerable caches. Dry-run unless --apply is passed.
    Reclaim {
        /// Actually delete (default is a dry run that deletes nothing).
        #[arg(long)]
        apply: bool,
        /// Skip the confirmation prompt (required with --apply when piped).
        #[arg(long)]
        yes: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";
const TOP_PER_LEVEL: usize = 12;

fn home() -> PathBuf {
    home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Commands::Scan {
            path,
            top,
            depth,
            signatures,
            no_tree,
            json,
        } => {
            let root = path.unwrap_or_else(home);
            let signatures = match signatures {
                Some(path) => load_signatures(&path)?,
                None => default_signatures(),
            };
            let depth = depth.clamp(1, 6);
            let report = build_report(&root, top, !no_tree, depth, &signatures)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_scan(&report);
            }
            Ok(())
        }
        Commands::Anomalies { path, json } => run_anomalies(path, json),
        Commands::Reclaim { apply, yes, json } => run_reclaim(apply, yes, json),
    }
}

fn run_anomalies(path: Option<PathBuf>, json: bool) -> anyhow::Result<()> {
    let anomalies = match path {
        Some(path) => {
            let mut found = analyze_container("custom", &path, 100 * 1024 * 1024);
            found.sort_by_key(|anomaly| std::cmp::Reverse(anomaly.bytes));
            found
        }
        None => detect_anomalies(&default_zones()),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&anomalies)?);
        return Ok(());
    }

    println!(
        "{BOLD}deepscan anomalies{RESET} {DIM}· size outliers vs sibling median (learned baseline){RESET}"
    );
    if anomalies.is_empty() {
        println!("  {DIM}no outliers — every sibling looks normal{RESET}");
        return Ok(());
    }
    for anomaly in &anomalies {
        let (color, tag) = match anomaly.severity {
            Severity::Critical => (RED, "CRIT"),
            Severity::Warn => (YELLOW, "WARN"),
            Severity::Info => (DIM, "INFO"),
        };
        println!(
            "  {color}\u{26a0} [{tag}] {}{RESET} {BOLD}{}{RESET} {DIM}in {}{RESET}",
            anomaly.name,
            human(anomaly.bytes),
            anomaly.zone
        );
        match anomaly.ratio {
            Some(ratio) => println!(
                "      {:.0}\u{00d7} the sibling median ({}) across {} siblings",
                ratio,
                human(anomaly.median_bytes),
                anomaly.siblings
            ),
            None => println!(
                "      lone outlier — sibling median is ~0 across {} siblings",
                anomaly.siblings
            ),
        }
        println!("      path: {}", anomaly.path.display());
    }
    Ok(())
}

fn render_scan(report: &ScanReport) {
    println!(
        "{BOLD}deepscan{RESET} {DIM}· scanning {}{RESET}",
        report.root.display()
    );

    if let Some(tree) = &report.tree {
        println!(
            "\n{BOLD}Where the space is{RESET} {DIM}(total {}){RESET}",
            human(report.total_bytes)
        );
        // Prune small entries so a deep tree stays readable.
        let threshold = (report.total_bytes / 100).max(50 * 1024 * 1024);
        let shown = tree.children.iter().filter(|c| c.bytes >= threshold);
        for child in shown.take(TOP_PER_LEVEL) {
            print_tree(child, 1, threshold);
        }
    } else if !report.children.is_empty() {
        println!(
            "\n{BOLD}Where the space is{RESET} {DIM}(total {}){RESET}",
            human(report.total_bytes)
        );
        for child in &report.children {
            println!("  {:>11}  {}", human(child.bytes), child.name);
        }
    }

    if !report.buckets.is_empty() {
        println!(
            "\n{BOLD}Reclaimable buckets{RESET} {DIM}({} across {} locations){RESET}",
            human(report.reclaimable_bytes),
            report.buckets.len()
        );
        for bucket in &report.buckets {
            println!(
                "  {:>11}  {CYAN}{}{RESET} {DIM}— {}{RESET}",
                human(bucket.bytes),
                bucket.name,
                bucket.note
            );
        }
    }

    println!("\n{BOLD}Leak signatures{RESET}");
    if report.findings.is_empty() {
        println!("  {DIM}no anomalies above baseline — clean{RESET}");
    } else {
        for finding in &report.findings {
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
}

fn print_tree(node: &TreeNode, indent: usize, threshold: u64) {
    let pad = "  ".repeat(indent);
    println!("  {:>11}  {pad}{}", human(node.bytes), node.name);

    let shown: Vec<&TreeNode> = node
        .children
        .iter()
        .filter(|child| child.bytes >= threshold)
        .collect();
    for child in shown.iter().take(TOP_PER_LEVEL) {
        print_tree(child, indent + 1, threshold);
    }
    let hidden = shown.len().saturating_sub(TOP_PER_LEVEL);
    if hidden > 0 {
        let pad = "  ".repeat(indent + 1);
        println!("  {:>11}  {pad}{DIM}… {hidden} more{RESET}", "");
    }
}

fn run_reclaim(apply: bool, yes: bool, json: bool) -> anyhow::Result<()> {
    let plan = build_reclaim_plan();

    if !apply {
        if json {
            println!("{}", serde_json::to_string_pretty(&plan)?);
        } else {
            render_plan(&plan, true);
        }
        return Ok(());
    }

    // --apply: confirm before deleting.
    if plan.auto_targets.is_empty() {
        if !json {
            println!("{DIM}Nothing safe to reclaim.{RESET}");
        }
        return Ok(());
    }

    if !yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("refusing to delete without confirmation — re-run with --apply --yes");
        }
        if !json {
            render_plan(&plan, false);
        }
        print!(
            "\nDelete {} of regenerable caches? [y/N] ",
            human(plan.auto_bytes)
        );
        std::io::stdout().flush().ok();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Aborted. Nothing deleted.");
            return Ok(());
        }
    }

    let result = execute_reclaim(&plan.auto_targets, home_dir().as_deref());

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("\n{BOLD}deepscan reclaim{RESET} {DIM}· applying{RESET}");
        for outcome in &result.deleted {
            if outcome.ok {
                println!(
                    "  {GREEN}freed{RESET}  {:>11}  {}",
                    human(outcome.bytes),
                    outcome.name
                );
            } else {
                let err = outcome.error.as_deref().unwrap_or("failed");
                println!(
                    "  {RED}skip{RESET}   {:>11}  {} {DIM}({err}){RESET}",
                    human(outcome.bytes),
                    outcome.name
                );
            }
        }
        println!("\n{BOLD}Reclaimed {}{RESET}", human(result.freed_bytes));
    }
    Ok(())
}

fn render_plan(plan: &ReclaimPlan, with_hint: bool) {
    println!("{BOLD}deepscan reclaim{RESET} {DIM}· dry run (nothing deleted){RESET}");

    if plan.auto_targets.is_empty() {
        println!("\n{DIM}Nothing safe to reclaim automatically.{RESET}");
    } else {
        println!(
            "\n{BOLD}Safe to reclaim{RESET} {DIM}({} across {} targets){RESET}",
            human(plan.auto_bytes),
            plan.auto_targets.len()
        );
        for target in &plan.auto_targets {
            println!(
                "  {GREEN}{:>11}{RESET}  {}  {DIM}— {}{RESET}",
                human(target.bytes),
                target.name,
                target.note
            );
        }
    }

    if !plan.manual_targets.is_empty() {
        println!("\n{BOLD}Manual{RESET} {DIM}— review and handle yourself{RESET}");
        for target in &plan.manual_targets {
            println!(
                "  {:>11}  {}  {DIM}— {}{RESET}",
                human(target.bytes),
                target.name,
                target.note
            );
        }
    }

    if with_hint && !plan.auto_targets.is_empty() {
        println!(
            "\n{DIM}Run{RESET} deepscan reclaim --apply {DIM}to free the {} of safe targets.{RESET}",
            human(plan.auto_bytes)
        );
    }
}
