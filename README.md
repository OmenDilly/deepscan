# deepscan

[![CI](https://github.com/OmenDilly/deepscan/actions/workflows/ci.yml/badge.svg)](https://github.com/OmenDilly/deepscan/actions/workflows/ci.yml)

Fast macOS disk forensics. A parallel size scanner with **broad coverage**
(the reclaimable buckets every cleaner knows about) plus a **leak-signature
engine** that catches the bloat nothing else does — like the `idleassetsd`
aerial-wallpaper leak that can quietly hoard *hundreds of gigabytes* of
abandoned downloads in your per-user temp directory, invisible to Finder,
DaisyDisk, and CleanMyMac alike.

> The treemap apps show you a *big folder*. deepscan tells you *which daemon
> leaked it, whether that's abnormal, how to reclaim it safely, and how to
> stop it coming back.*

## Install

```sh
# Homebrew (macOS)
brew install OmenDilly/deepscan/deepscan

# or with cargo (needs Rust via rustup.rs)
cargo install --git https://github.com/OmenDilly/deepscan deepscan-cli
```

Then `deepscan scan` works anywhere. To hack on it, clone and use `cargo run`
as shown below.

## Quick start

```sh
deepscan scan                 # fast (~seconds): reclaimable space + leak signatures
deepscan scan --tree          # also walk the whole tree — where did the space go?
deepscan scan ~/Library/Developer --depth 3   # nested size tree of a directory
deepscan scan --json | jq .   # machine-readable, for scripts/CI
deepscan explore              # interactive size explorer (navigate with arrow keys)

deepscan space                # honest disk accounting: capacity + the "System Data" gap
deepscan large --older 90     # biggest files you haven't modified in 90+ days
deepscan dupes                # exact duplicate files (zero false positives)
deepscan uninstall Docker     # an app + its leftover files (dry-run; --apply trashes)

deepscan anomalies            # find UNKNOWN leaks: outliers vs sibling median
deepscan anomalies "$TMPDIR"  # analyze any one directory's children

deepscan reclaim              # dry run: what's safe to free (deletes nothing)
deepscan reclaim --apply      # free regenerable caches (asks first)
```

> `scan` is fast by default because the full "where did it go" tree walk
> traverses *every* file under the root (millions, on a dev machine — a
> `du`-level operation). It's opt-in via `--tree` / `--depth`, and shows a live
> progress spinner while it runs.

## Commands

| Command | What it does |
|---|---|
| `deepscan scan [PATH]` | Reclaimable caches + leak signatures (fast). `--tree` / `--depth N` adds the full size tree; `--json` for machine output. |
| `deepscan anomalies [PATH]` | Unknown-leak detection — directories that are size outliers vs their siblings' learned median. |
| `deepscan reclaim` | Guarded cleanup of regenerable caches. Dry-run by default; `--apply` (with `--yes`) to delete. |
| `deepscan space [PATH]` | Honest disk accounting — true capacity/used/free + the local APFS snapshots behind "System Data" (with the reclaim command). |
| `deepscan large [PATH]` | Largest files, optionally only old ones (`--older N`, `--min-mb`). |
| `deepscan dupes [PATH]` | Exact duplicate files (size-bucket + BLAKE3; zero false positives). |
| `deepscan uninstall <APP>` | An app + its leftover support files (confidence-tiered). Dry-run; `--apply` moves them to the Trash (recoverable). |
| `deepscan explore [PATH]` | Interactive size explorer — drill the tree with the arrow keys (a "CLI DaisyDisk"). `↑↓` move · `→` enter · `←` back · `q` quit. |

Run `deepscan help <command>` (or `<command> --help`) for the full flag list.
Every command supports `--json`.

**Scripting:** `scan` and `anomalies` accept `--exit-code` (exit `1` on
warnings, `2` on critical) so they gate CI. `reclaim --only <name>` (repeatable)
limits cleanup to targets whose name matches.

```sh
deepscan scan --exit-code || echo "leaks found!"
deepscan reclaim --only npm --only cargo --apply --yes
```

## What it reports

1. **Reclaimable buckets** — known caches (Xcode, npm/yarn/pnpm/cargo/go,
   CocoaPods, Trash, …), always sized.
2. **Leak signatures** — *known* leaks above a baseline, each with owner, root
   cause, prevention, and a safe reclaim command (printed, never auto-run).
3. **Where the space is** (`--tree` / `--depth N`) — a nested size tree of the
   scanned root. Opt-in, since it walks every file under the root.
4. **Anomalies** (`deepscan anomalies`) — *unknown* leaks: directories that are
   size outliers vs their siblings' learned median. Catches the next
   idleassetsd-style leak with no signature written for it.
5. **Honest disk accounting** (`deepscan space`) — true capacity/used/free plus
   the local APFS snapshots behind the dreaded "System Data" figure (with the
   reclaim command). macOS doesn't expose per-snapshot sizes, so deepscan says
   so plainly instead of faking a number — the honest answer no other tool gives.

## Signatures are data, not code

The leak rules live in [`signatures.toml`](signatures.toml). Add a
`[[signature]]` block to teach deepscan a new leak, or pass your own set with
`--signatures my-rules.toml`. PRs that add real-world leaks are the point.

## Architecture

- `deepscan-core` — the engine: parallel sizing, the catalog, the signature
  evaluator. Designed to later build as a `cdylib` so a native Swift
  menu-bar app can reuse the exact same fast core via a C ABI.
- `deepscan-cli` — the `deepscan` binary.

## Performance

The scanner is a `rayon::scope` parallel walk. On macOS each directory is
listed with **`getattrlistbulk(2)`** — one syscall returns every child's name,
type, and size, replacing the per-file `stat()` loop. Measured vs the portable
`stat` backend (identical totals, so it's a pure speedup): **~13% faster on
directory-dense trees, ~20–35% on file-dense ones**; the win scales with
files-per-directory and with a cold cache. Set `DEEPSCAN_NO_BULK=1` to force
the portable backend (handy for A/B benchmarking, or as a safety valve).

## Roadmap

- [x] `getattrlistbulk(2)` fast path — one syscall per directory instead of
      one stat per file.
- [x] `--json` output and a guarded `reclaim --apply` mode.
- [x] Baseline learning (v1) — flag size outliers vs the *learned sibling
      median*, catching unknown leaks with no signature.
- [ ] Community baselines — compare against a cross-machine median per
      directory, not just local siblings.
- [ ] Swift menu-bar app over the shared core.

## Safety

By default deepscan **never deletes anything** — `scan` is read-only and
`reclaim` is a dry run. Deletion happens only through the explicit, guarded
`reclaim --apply`:

- Only **regenerable caches under `$HOME`** are ever auto-deleted. User files
  (Downloads), app state (simulator devices), cross-project stores (pnpm
  store), and anything needing `sudo` (system leak paths) are listed as
  **Manual** and never touched automatically.
- A safety guard refuses the filesystem root, near-root paths, the home
  directory itself, and any path containing `..`.
- `--apply` prompts for confirmation; pass `--yes` to skip it in scripts — and
  it refuses to run unattended (piped) without `--yes`.

Leak signatures that target system paths print the `sudo` command for you to
review and run, and call out what must stay intact.

## License

MIT
