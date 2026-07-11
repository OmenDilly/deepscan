//! deepscan — fast macOS disk forensics.
//!
//!   deepscan scan [PATH]      reclaimable + leaks (+ --tree for where it went)
//!   deepscan anomalies [PATH] size outliers vs sibling median (unknown leaks)
//!   deepscan clean [--apply]    guarded cleanup of regenerable caches (alias: reclaim)

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use deepscan_core::{
    analyze_container, build_reclaim_plan, build_report, default_signatures, default_zones,
    detect_anomalies, execute_reclaim, execute_uninstall, find_duplicates, find_large_files,
    home_dir, human, load_signatures, plan_uninstall, progress, reset_progress, space_report,
    AnomalyKind, Confidence, DuplicateGroup, ReclaimPlan, ScanReport, Severity, SpaceReport,
    TreeNode, UninstallPlan,
};

mod tui;

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
    /// Scan for reclaimable space + leaks (fast). Add --tree to also walk the
    /// whole tree and show where space went.
    Scan {
        /// Root to scan (default: your home directory).
        path: Option<PathBuf>,
        /// Show the top N largest children (with --tree / --depth).
        #[arg(long, default_value_t = 12)]
        top: usize,
        /// Walk the full tree to show where space went, to this depth.
        /// 0 (default) skips the slow full-home walk; N >= 1 implies --tree.
        #[arg(long, default_value_t = 0)]
        depth: usize,
        /// Show "where the space is" — walks the whole tree (slower).
        #[arg(long)]
        tree: bool,
        /// Use a custom signatures file instead of the built-in set.
        #[arg(long)]
        signatures: Option<PathBuf>,
        /// Emit machine-readable JSON instead of the formatted report.
        #[arg(long)]
        json: bool,
        /// Exit non-zero when leaks are found (1 = warnings, 2 = critical).
        #[arg(long)]
        exit_code: bool,
        /// Force the plain printed report even in a terminal.
        #[arg(long)]
        plain: bool,
    },
    /// Biggest same-class folders, classified: caches (safe) vs app data (review).
    Anomalies {
        /// Analyze one directory's children instead of the default zones.
        path: Option<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Exit non-zero when large app-data folders are found (1 = warn, 2 = critical).
        #[arg(long)]
        exit_code: bool,
    },
    /// Reclaim regenerable caches. Dry-run unless --apply is passed.
    #[command(alias = "reclaim")]
    Clean {
        /// Actually delete (default is a dry run that deletes nothing).
        #[arg(long)]
        apply: bool,
        /// Skip the confirmation prompt (required with --apply when piped).
        #[arg(long)]
        yes: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Limit to targets whose name contains this text (repeatable).
        #[arg(long, value_name = "NAME")]
        only: Vec<String>,
        /// Force the plain printed report even in a terminal.
        #[arg(long)]
        plain: bool,
    },
    /// Honest disk accounting: true capacity + local snapshots (the "System Data" gap).
    Space {
        /// Volume or path to report on (default: your home directory).
        path: Option<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// List the largest files, optionally only old ones.
    Large {
        /// Root to search (default: your home directory).
        path: Option<PathBuf>,
        /// Show the top N files.
        #[arg(long, default_value_t = 12)]
        top: usize,
        /// Minimum file size to consider, in MB.
        #[arg(long, default_value_t = 100)]
        min_mb: u64,
        /// Only files not modified in at least N days.
        #[arg(long)]
        older: Option<u64>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Force the plain printed report even in a terminal.
        #[arg(long)]
        plain: bool,
    },
    /// Find exact duplicate files (size-bucket + BLAKE3, zero false positives).
    Dupes {
        /// Root to search (default: your home directory).
        path: Option<PathBuf>,
        /// Show the top N duplicate sets.
        #[arg(long, default_value_t = 12)]
        top: usize,
        /// Minimum file size to consider, in MB.
        #[arg(long, default_value_t = 1)]
        min_mb: u64,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Force the plain printed report even in a terminal.
        #[arg(long)]
        plain: bool,
    },
    /// Find an app + its leftover files; dry-run, or --apply to Trash them.
    Uninstall {
        /// App name (e.g. "Docker") or a path to a .app bundle.
        app: String,
        /// Move the app + leftovers to the Trash (recoverable). Default: dry-run.
        #[arg(long)]
        apply: bool,
        /// Skip the confirmation prompt (required with --apply when piped).
        #[arg(long)]
        yes: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Interactively explore where the space went (a "CLI DaisyDisk").
    Explore {
        /// Root to explore (default: your home directory).
        path: Option<PathBuf>,
    },
}

const TOP_PER_LEVEL: usize = 12;

struct Palette {
    bold: &'static str,
    dim: &'static str,
    red: &'static str,
    green: &'static str,
    yellow: &'static str,
    cyan: &'static str,
    reset: &'static str,
}

/// ANSI palette. Colors are emitted only when stdout is a TTY and `NO_COLOR` is
/// unset, so piped or redirected output stays plain text.
fn palette() -> Palette {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    let enabled = *ENABLED
        .get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none());
    if enabled {
        Palette {
            bold: "\x1b[1m",
            dim: "\x1b[2m",
            red: "\x1b[31m",
            green: "\x1b[32m",
            yellow: "\x1b[33m",
            cyan: "\x1b[36m",
            reset: "\x1b[0m",
        }
    } else {
        Palette {
            bold: "",
            dim: "",
            red: "",
            green: "",
            yellow: "",
            cyan: "",
            reset: "",
        }
    }
}

fn home() -> PathBuf {
    home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Launch the interactive TUI only on a real terminal, and only when the user
/// hasn't asked for machine/plain output.
fn interactive(json: bool, plain: bool) -> bool {
    !json && !plain && std::io::stdout().is_terminal()
}

/// Report the result of an interactive Trash: freed total, then any per-item
/// failures (guard refusals, permission/SIP errors) so a partial failure is
/// never silent.
fn report_trash(outcome: &tui::TrashOutcome) {
    if outcome.freed_bytes > 0 {
        println!("Moved {} to the Trash.", human(outcome.freed_bytes));
    }
    for (path, err) in &outcome.failures {
        eprintln!("skip  {}  ({err})", path.display());
    }
}

/// Map the highest finding severity to a process exit code: 2 critical,
/// 1 warning, 0 otherwise (clean, or info-only).
fn exit_code_for(max: Option<Severity>) -> ExitCode {
    match max {
        Some(Severity::Critical) => ExitCode::from(2),
        Some(Severity::Warn) => ExitCode::from(1),
        _ => ExitCode::SUCCESS,
    }
}

/// Keep only reclaim targets whose name contains one of `patterns` (case-insensitive).
fn filter_plan(plan: ReclaimPlan, patterns: &[String]) -> ReclaimPlan {
    let lowered: Vec<String> = patterns.iter().map(|p| p.to_lowercase()).collect();
    let matches = |name: &str| {
        let name = name.to_lowercase();
        lowered.iter().any(|p| name.contains(p.as_str()))
    };
    let auto_targets: Vec<_> = plan
        .auto_targets
        .into_iter()
        .filter(|t| matches(&t.name))
        .collect();
    let manual_targets: Vec<_> = plan
        .manual_targets
        .into_iter()
        .filter(|t| matches(&t.name))
        .collect();
    let auto_bytes = auto_targets.iter().map(|t| t.bytes).sum();
    ReclaimPlan {
        auto_targets,
        manual_targets,
        auto_bytes,
    }
}

/// Run `work` on a worker thread while animating a live progress line on
/// stderr (TTY only — `--json` on stdout stays clean, pipes/CI stay quiet).
fn with_spinner<T, F>(label: &str, work: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    use std::sync::mpsc;
    use std::time::Duration;

    let show = std::io::stderr().is_terminal();
    reset_progress();

    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = tx.send(work());
    });

    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut frame = 0usize;
    let value = loop {
        match rx.recv_timeout(Duration::from_millis(120)) {
            Ok(value) => break value,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if show {
                    let (dirs, bytes) = progress();
                    eprint!(
                        "\r\x1b[2K{} {label}… {} scanned · {} dirs",
                        FRAMES[frame % FRAMES.len()],
                        human(bytes),
                        dirs
                    );
                    let _ = std::io::stderr().flush();
                    frame += 1;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                worker.join().expect("scan worker panicked");
                unreachable!("worker disconnected without panicking");
            }
        }
    };

    let _ = worker.join();
    if show {
        eprint!("\r\x1b[2K");
        let _ = std::io::stderr().flush();
    }
    value
}

fn main() -> anyhow::Result<ExitCode> {
    match Cli::parse().command {
        Commands::Scan {
            path,
            top,
            depth,
            tree,
            signatures,
            json,
            exit_code,
            plain,
        } => {
            let root = path.unwrap_or_else(home);
            let signatures = match signatures {
                Some(path) => load_signatures(&path)?,
                None => default_signatures(),
            };
            let depth = if tree { depth.max(1) } else { depth };
            let depth = depth.min(6);
            let include_tree = depth >= 1;
            if interactive(json, plain) {
                // The dashboard does extra work (anomalies + biggest files) the
                // fast default scan skips — fine, opening it is interactive.
                let report = build_report(&root, top, false, 0, &signatures)?;
                let root_for_walk = root.clone();
                let (anomalies, largest, snapshots) = with_spinner("scanning", move || {
                    let anomalies = detect_anomalies(&default_zones());
                    let mut largest = find_large_files(&root_for_walk, 100 * 1024 * 1024);
                    largest.truncate(12);
                    let snapshots = space_report(&root_for_walk).snapshots.len();
                    (anomalies, largest, snapshots)
                });
                let dashboard =
                    tui::assemble_dashboard(root, &report, &anomalies, &largest, snapshots);
                tui::run_dashboard(dashboard)?;
                return Ok(ExitCode::SUCCESS);
            }
            let report = with_spinner("scanning", move || {
                build_report(&root, top, include_tree, depth, &signatures)
            })?;
            let code = if exit_code {
                exit_code_for(report.findings.iter().map(|f| f.severity).max())
            } else {
                ExitCode::SUCCESS
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_scan(&report);
            }
            Ok(code)
        }
        Commands::Anomalies {
            path,
            json,
            exit_code,
        } => run_anomalies(path, json, exit_code),
        Commands::Clean {
            apply,
            yes,
            json,
            only,
            plain,
        } => {
            run_clean(apply, yes, json, only, plain)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Space { path, json } => {
            let root = path.unwrap_or_else(home);
            let report = space_report(&root);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_space(&root, &report);
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Large {
            path,
            top,
            min_mb,
            older,
            json,
            plain,
        } => run_large(path, top, min_mb, older, json, plain),
        Commands::Dupes {
            path,
            top,
            min_mb,
            json,
            plain,
        } => run_dupes(path, top, min_mb, json, plain),
        Commands::Uninstall {
            app,
            apply,
            yes,
            json,
        } => run_uninstall(app, apply, yes, json),
        Commands::Explore { path } => {
            tui::run(path.unwrap_or_else(home))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run_large(
    path: Option<PathBuf>,
    top: usize,
    min_mb: u64,
    older: Option<u64>,
    json: bool,
    plain: bool,
) -> anyhow::Result<ExitCode> {
    let root = path.unwrap_or_else(home);
    let min = min_mb.saturating_mul(1024 * 1024);
    let mut files = with_spinner("finding large files", move || find_large_files(&root, min));
    if let Some(days) = older {
        files.retain(|f| f.modified_days.map(|d| d >= days).unwrap_or(false));
    }
    files.truncate(top);

    if interactive(json, plain) && !files.is_empty() {
        let rows: Vec<tui::Row> = files
            .iter()
            .map(|f| {
                tui::Row::new(
                    f.bytes,
                    f.path.display().to_string(),
                    f.path.clone(),
                    tui::RowMeta::Age(f.modified_days),
                )
            })
            .collect();
        let title = match older {
            Some(days) => format!("deepscan large · not modified in {days}+ days"),
            None => "deepscan large".to_string(),
        };
        let outcome = tui::run_list(
            rows,
            tui::Sort::Size,
            tui::ListConfig {
                title,
                home: home_dir(),
            },
        )?;
        report_trash(&outcome);
        return Ok(ExitCode::SUCCESS);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&files)?);
        return Ok(ExitCode::SUCCESS);
    }

    let Palette {
        bold,
        dim,
        cyan,
        reset,
        ..
    } = palette();
    let label = match older {
        Some(days) => format!("largest files not modified in {days}+ days"),
        None => "largest files".to_string(),
    };
    println!("{bold}deepscan large{reset} {dim}· {label}{reset}");
    if files.is_empty() {
        println!("  {dim}nothing above the size threshold{reset}");
        return Ok(ExitCode::SUCCESS);
    }
    for file in &files {
        let age = file
            .modified_days
            .map(|d| format!("{d}d"))
            .unwrap_or_else(|| "?".to_string());
        println!(
            "  {cyan}{:>11}{reset}  {dim}{:>5} old{reset}  {}",
            human(file.bytes),
            age,
            file.path.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn run_dupes(
    path: Option<PathBuf>,
    top: usize,
    min_mb: u64,
    json: bool,
    plain: bool,
) -> anyhow::Result<ExitCode> {
    let root = path.unwrap_or_else(home);
    let min = min_mb.saturating_mul(1024 * 1024);
    let groups = with_spinner("finding duplicates", move || find_duplicates(&root, min));

    if interactive(json, plain) && !groups.is_empty() {
        let mut rows: Vec<tui::Row> = Vec::new();
        for (gid, group) in groups.iter().take(top).enumerate() {
            for path in &group.paths {
                rows.push(tui::Row {
                    bytes: group.bytes,
                    label: path.display().to_string(),
                    path: path.clone(),
                    selected: false,
                    meta: tui::RowMeta::Tag(format!("×{}", group.paths.len())),
                    group: Some(gid),
                });
            }
        }
        let title = format!(
            "deepscan dupes · {} reclaimable",
            human(groups.iter().map(|g| g.wasted).sum())
        );
        let outcome = tui::run_list(
            rows,
            tui::Sort::Size,
            tui::ListConfig {
                title,
                home: home_dir(),
            },
        )?;
        report_trash(&outcome);
        return Ok(ExitCode::SUCCESS);
    }

    if json {
        let shown: Vec<&DuplicateGroup> = groups.iter().take(top).collect();
        println!("{}", serde_json::to_string_pretty(&shown)?);
        return Ok(ExitCode::SUCCESS);
    }

    let Palette {
        bold,
        dim,
        green,
        reset,
        ..
    } = palette();
    let wasted: u64 = groups.iter().map(|g| g.wasted).sum();
    println!(
        "{bold}deepscan dupes{reset} {dim}· {} reclaimable across {} duplicate set{}{reset}",
        human(wasted),
        groups.len(),
        if groups.len() == 1 { "" } else { "s" }
    );
    if groups.is_empty() {
        println!("  {dim}no exact duplicates found{reset}");
        return Ok(ExitCode::SUCCESS);
    }
    for group in groups.iter().take(top) {
        println!(
            "  {green}{:>11}{reset} wasted  {dim}({} copies × {}){reset}",
            human(group.wasted),
            group.paths.len(),
            human(group.bytes)
        );
        for path in &group.paths {
            println!("      {dim}{}{reset}", path.display());
        }
    }
    let hidden = groups.len().saturating_sub(top);
    if hidden > 0 {
        println!("  {dim}… {hidden} more set(s){reset}");
    }
    Ok(ExitCode::SUCCESS)
}

fn run_uninstall(app: String, apply: bool, yes: bool, json: bool) -> anyhow::Result<ExitCode> {
    let plan = with_spinner("finding app + leftovers", move || plan_uninstall(&app));
    let plan = match plan {
        Some(plan) => plan,
        None => anyhow::bail!("app not found — pass the exact name or a path to the .app bundle"),
    };

    if !apply {
        if interactive(json, false) {
            let mut rows: Vec<tui::Row> = Vec::new();
            if let Some(app) = &plan.app_path {
                let mut r = tui::Row::new(
                    plan.app_bytes,
                    app.display().to_string(),
                    app.clone(),
                    tui::RowMeta::Confidence("app".into()),
                );
                r.selected = true;
                rows.push(r);
            }
            for l in &plan.leftovers {
                let tag = match l.confidence {
                    Confidence::High => "high",
                    Confidence::Medium => "med?",
                };
                let mut r = tui::Row::new(
                    l.bytes,
                    l.path.display().to_string(),
                    l.path.clone(),
                    tui::RowMeta::Confidence(tag.into()),
                );
                r.selected = true;
                rows.push(r);
            }
            let outcome = tui::run_list(
                rows,
                tui::Sort::Size,
                tui::ListConfig {
                    title: format!("deepscan uninstall · {}", plan.app_name),
                    home: home_dir(),
                },
            )?;
            report_trash(&outcome);
            return Ok(ExitCode::SUCCESS);
        }
        if json {
            println!("{}", serde_json::to_string_pretty(&plan)?);
        } else {
            render_uninstall(&plan);
            let Palette { dim, reset, .. } = palette();
            println!("\n  {dim}--apply moves all of the above to the Trash (recoverable).{reset}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // --apply: show what will move, then confirm.
    if !json {
        render_uninstall(&plan);
    }
    if plan.app_path.is_none() && plan.leftovers.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("refusing to uninstall without confirmation — re-run with --apply --yes");
        }
        print!(
            "\nMove {} ({}) to the Trash? [y/N] ",
            plan.app_name,
            human(plan.total_bytes)
        );
        std::io::stdout().flush().ok();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Aborted. Nothing moved.");
            return Ok(ExitCode::SUCCESS);
        }
    }

    let outcomes = execute_uninstall(&plan);
    if json {
        println!("{}", serde_json::to_string_pretty(&outcomes)?);
    } else {
        let Palette {
            bold,
            dim,
            red,
            green,
            reset,
            ..
        } = palette();
        let freed: u64 = outcomes.iter().filter(|o| o.ok).map(|o| o.bytes).sum();
        for outcome in &outcomes {
            if outcome.ok {
                println!(
                    "  {green}trashed{reset}  {:>11}  {}",
                    human(outcome.bytes),
                    outcome.path.display()
                );
            } else {
                let err = outcome.error.as_deref().unwrap_or("failed");
                println!(
                    "  {red}skip{reset}     {:>11}  {} {dim}({err}){reset}",
                    human(outcome.bytes),
                    outcome.path.display()
                );
            }
        }
        println!("\n  {bold}Moved {} to the Trash.{reset}", human(freed));
    }
    Ok(ExitCode::SUCCESS)
}

fn render_uninstall(plan: &UninstallPlan) {
    let Palette {
        bold,
        dim,
        cyan,
        yellow,
        green,
        reset,
        ..
    } = palette();

    println!(
        "{bold}deepscan uninstall{reset} {dim}· {}{reset}",
        plan.app_name
    );
    if let Some(bundle_id) = &plan.bundle_id {
        println!("  {dim}bundle id: {bundle_id}{reset}");
    }
    if let Some(app) = &plan.app_path {
        println!(
            "  {cyan}{:>11}{reset}  {dim}app{reset}     {}",
            human(plan.app_bytes),
            app.display()
        );
    } else {
        println!("  {dim}(app bundle not found — showing matched leftovers only){reset}");
    }

    if plan.leftovers.is_empty() {
        println!("  {dim}no leftover files found{reset}");
    } else {
        for leftover in &plan.leftovers {
            let (label, color) = match leftover.confidence {
                Confidence::High => ("high", green),
                Confidence::Medium => ("med?", yellow),
            };
            println!(
                "  {:>11}  {color}[{label}]{reset} {dim}{}{reset}  {}",
                human(leftover.bytes),
                leftover.location,
                leftover.path.display()
            );
        }
    }

    println!("\n  {bold}total: {}{reset}", human(plan.total_bytes));
}

fn render_space(root: &Path, report: &SpaceReport) {
    let Palette {
        bold,
        dim,
        cyan,
        reset,
        ..
    } = palette();

    println!(
        "{bold}deepscan space{reset} {dim}· {}{reset}",
        root.display()
    );

    if let Some(disk) = &report.disk {
        let pct = (disk.used * 100).checked_div(disk.total).unwrap_or(0);
        println!("  {:>11}  capacity", human(disk.total));
        println!("  {:>11}  used  {dim}({pct}%){reset}", human(disk.used));
        println!("  {cyan}{:>11}{reset}  free", human(disk.free));
    } else {
        println!("  {dim}(disk capacity unavailable on this platform){reset}");
    }

    println!();
    if report.snapshots.is_empty() {
        println!("  {dim}no local APFS snapshots{reset}");
    } else {
        println!(
            "  {bold}Local snapshots: {}{reset} {dim}(macOS does not expose per-snapshot sizes){reset}",
            report.snapshots.len()
        );
        for snapshot in &report.snapshots {
            println!("    {dim}{snapshot}{reset}");
        }
        if let Some(cmd) = &report.reclaim_snapshots {
            println!("  reclaim:  {cmd}");
        }
    }

    println!();
    println!(
        "  {dim}The \"System Data\" gap = these snapshots + purgeable caches macOS reclaims{reset}"
    );
    println!("  {dim}on demand. Their exact sizes are not exposed by tmutil/diskutil — that's the{reset}");
    println!("  {dim}honest limit. Run `deepscan scan --tree` to see the files you CAN account for.{reset}");
}

fn run_anomalies(path: Option<PathBuf>, json: bool, exit_code: bool) -> anyhow::Result<ExitCode> {
    let anomalies = match path {
        Some(path) => with_spinner("analyzing", move || {
            let mut found = analyze_container("custom", &path, 100 * 1024 * 1024);
            found.sort_by_key(|anomaly| std::cmp::Reverse(anomaly.bytes));
            found
        }),
        None => with_spinner("analyzing zones", || detect_anomalies(&default_zones())),
    };

    let code = if exit_code {
        exit_code_for(anomalies.iter().map(|a| a.severity).max())
    } else {
        ExitCode::SUCCESS
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&anomalies)?);
        return Ok(code);
    }

    let Palette {
        bold,
        dim,
        red,
        yellow,
        green,
        reset,
        ..
    } = palette();

    println!(
        "{bold}deepscan anomalies{reset} {dim}· biggest same-class folders — app data to review vs caches you can clear{reset}"
    );
    if anomalies.is_empty() {
        println!("  {dim}nothing notably large in the scanned zones{reset}");
        return Ok(code);
    }

    // App data is the real signal (a big unexplained one is worth a look);
    // caches are just big-and-safe. Both stay largest-first from detection.
    let (app_data, caches): (Vec<_>, Vec<_>) = anomalies
        .iter()
        .partition(|anomaly| anomaly.kind == AnomalyKind::AppData);

    if !app_data.is_empty() {
        println!(
            "\n{bold}App data{reset} {dim}— real state; review what's inside before removing{reset}"
        );
        for anomaly in &app_data {
            let (color, tag) = match anomaly.severity {
                Severity::Critical => (red, "CRIT"),
                Severity::Warn => (yellow, "WARN"),
                Severity::Info => (dim, "INFO"),
            };
            println!(
                "  {color}[{tag}]{reset} {bold}{:>9}{reset}  {}  {dim}in {}{reset}",
                human(anomaly.bytes),
                anomaly.name,
                anomaly.zone
            );
            println!("      {dim}{}{reset}", anomaly.path.display());
        }
    }

    if !caches.is_empty() {
        let total: u64 = caches.iter().map(|anomaly| anomaly.bytes).sum();
        println!(
            "\n{bold}Caches{reset} {dim}— safe to clear, regenerate on next use ({} total · deepscan reclaim){reset}",
            human(total)
        );
        for anomaly in &caches {
            println!(
                "  {green}{:>9}{reset}  {}  {dim}in {}{reset}",
                human(anomaly.bytes),
                anomaly.name,
                anomaly.zone
            );
        }
    }
    Ok(code)
}

fn render_scan(report: &ScanReport) {
    let Palette {
        bold,
        dim,
        red,
        green,
        yellow,
        cyan,
        reset,
        ..
    } = palette();

    println!(
        "{bold}deepscan{reset} {dim}· scanning {}{reset}",
        report.root.display()
    );

    let critical = report
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .count();
    // Split the catalog by whether it's safe to auto-reclaim (regenerable
    // caches) vs review (user files, simulator devices, linked stores). The old
    // single "reclaimable" total over-claimed by lumping your Downloads in.
    let safe_bytes: u64 = report
        .buckets
        .iter()
        .filter(|b| b.safe_auto)
        .map(|b| b.bytes)
        .sum();
    let review_bytes = report.reclaimable_bytes.saturating_sub(safe_bytes);
    let reclaim_summary = if report.reclaimable_bytes == 0 {
        "nothing reclaimable in known locations".to_string()
    } else if review_bytes == 0 {
        format!("{} safe to reclaim", human(safe_bytes))
    } else {
        format!(
            "{} safe to reclaim {dim}·{reset} {bold}{} to review{reset}",
            human(safe_bytes),
            human(review_bytes)
        )
    };
    println!(
        "{dim}summary:{reset} {bold}{reclaim_summary}{reset} {dim}·{reset} {} leak signature{}{}",
        report.findings.len(),
        if report.findings.len() == 1 { "" } else { "s" },
        if critical > 0 {
            format!(" ({critical} critical)")
        } else {
            String::new()
        }
    );
    if let Some(disk) = &report.disk {
        let pct = (disk.used * 100).checked_div(disk.total).unwrap_or(0);
        println!(
            "{dim}disk:{reset} {} used of {} {dim}· {} free ({pct}%){reset}",
            human(disk.used),
            human(disk.total),
            human(disk.free)
        );
    }

    if let Some(tree) = &report.tree {
        println!(
            "\n{bold}Where the space is{reset} {dim}(total {}){reset}",
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
            "\n{bold}Where the space is{reset} {dim}(total {}){reset}",
            human(report.total_bytes)
        );
        for child in &report.children {
            println!("  {:>11}  {}", human(child.bytes), child.name);
        }
    } else {
        println!(
            "\n{dim}Tip: add --tree (or --depth N) to see where all your space went — \
             it walks the whole tree, so it's slower.{reset}"
        );
    }

    if !report.buckets.is_empty() {
        println!(
            "\n{bold}Reclaimable buckets{reset} {dim}({} safe · {} review across {} locations){reset}",
            human(safe_bytes),
            human(review_bytes),
            report.buckets.len()
        );
        for bucket in &report.buckets {
            let (tag, tag_color) = if bucket.safe_auto {
                ("[safe]", green)
            } else {
                ("[review]", yellow)
            };
            println!(
                "  {:>11}  {tag_color}{:<8}{reset} {cyan}{}{reset} {dim}— {}{reset}",
                human(bucket.bytes),
                tag,
                bucket.name,
                bucket.note
            );
        }
    }

    println!("\n{bold}Leak signatures{reset}");
    if report.findings.is_empty() {
        println!("  {dim}no anomalies above baseline — clean{reset}");
    } else {
        for finding in &report.findings {
            let (color, tag) = match finding.severity {
                Severity::Critical => (red, "CRIT"),
                Severity::Warn => (yellow, "WARN"),
                Severity::Info => (dim, "INFO"),
            };
            println!(
                "  {color}\u{26a0} [{tag}] {}{reset} {bold}{}{reset} {dim}(baseline {}){reset}",
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
    let Palette { dim, reset, .. } = palette();
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
        println!("  {:>11}  {pad}{dim}… {hidden} more{reset}", "");
    }
}

fn run_clean(
    apply: bool,
    yes: bool,
    json: bool,
    only: Vec<String>,
    plain: bool,
) -> anyhow::Result<()> {
    let plan = with_spinner("scanning reclaimable", build_reclaim_plan);
    let plan = if only.is_empty() {
        plan
    } else {
        filter_plan(plan, &only)
    };

    let mut rows: Vec<tui::Row> = plan
        .auto_targets
        .iter()
        .map(|t| {
            tui::Row::new(
                t.bytes,
                t.name.clone(),
                t.path.clone(),
                tui::RowMeta::Tag("safe".into()),
            )
        })
        .collect();
    rows.extend(plan.manual_targets.iter().map(|t| {
        tui::Row::new(
            t.bytes,
            t.name.clone(),
            t.path.clone(),
            tui::RowMeta::Tag("review".into()),
        )
    }));

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

    if !apply {
        if json {
            println!("{}", serde_json::to_string_pretty(&plan)?);
        } else {
            render_plan(&plan, true);
        }
        return Ok(());
    }

    let Palette {
        bold,
        dim,
        red,
        green,
        reset,
        ..
    } = palette();

    // --apply: confirm before deleting.
    if plan.auto_targets.is_empty() {
        if !json {
            println!("{dim}Nothing safe to reclaim.{reset}");
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
        println!("\n{bold}deepscan clean{reset} {dim}· applying{reset}");
        for outcome in &result.deleted {
            if outcome.ok {
                println!(
                    "  {green}freed{reset}  {:>11}  {}",
                    human(outcome.bytes),
                    outcome.name
                );
            } else {
                let err = outcome.error.as_deref().unwrap_or("failed");
                println!(
                    "  {red}skip{reset}   {:>11}  {} {dim}({err}){reset}",
                    human(outcome.bytes),
                    outcome.name
                );
            }
        }
        println!("\n{bold}Reclaimed {}{reset}", human(result.freed_bytes));
    }
    Ok(())
}

fn render_plan(plan: &ReclaimPlan, with_hint: bool) {
    let Palette {
        bold,
        dim,
        green,
        reset,
        ..
    } = palette();

    println!("{bold}deepscan clean{reset} {dim}· dry run (nothing deleted){reset}");

    if plan.auto_targets.is_empty() {
        println!("\n{dim}Nothing safe to reclaim automatically.{reset}");
    } else {
        println!(
            "\n{bold}Safe to reclaim{reset} {dim}({} across {} targets){reset}",
            human(plan.auto_bytes),
            plan.auto_targets.len()
        );
        for target in &plan.auto_targets {
            println!(
                "  {green}{:>11}{reset}  {}  {dim}— {}{reset}",
                human(target.bytes),
                target.name,
                target.note
            );
        }
    }

    if !plan.manual_targets.is_empty() {
        println!("\n{bold}Manual{reset} {dim}— review and handle yourself{reset}");
        for target in &plan.manual_targets {
            println!(
                "  {:>11}  {}  {dim}— {}{reset}",
                human(target.bytes),
                target.name,
                target.note
            );
        }
    }

    if with_hint && !plan.auto_targets.is_empty() {
        println!(
            "\n{dim}Run{reset} deepscan reclaim --apply {dim}to free the {} of safe targets.{reset}",
            human(plan.auto_bytes)
        );
    }
}
