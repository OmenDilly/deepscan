# deepscan

Fast macOS disk forensics. A parallel size scanner with **broad coverage**
(the reclaimable buckets every cleaner knows about) plus a **leak-signature
engine** that catches the bloat nothing else does — like the `idleassetsd`
aerial-wallpaper leak that can quietly hoard *hundreds of gigabytes* of
abandoned downloads in your per-user temp directory, invisible to Finder,
DaisyDisk, and CleanMyMac alike.

> The treemap apps show you a *big folder*. deepscan tells you *which daemon
> leaked it, whether that's abnormal, how to reclaim it safely, and how to
> stop it coming back.*

## Quick start

```sh
cargo run --release -- scan          # scan your home directory
cargo run --release -- scan /        # scan from the volume root (run with sudo for shadow zones)
cargo run --release -- scan ~/Library/Developer --top 20
cargo run --release -- scan --json | jq .   # machine-readable, for scripts/CI

cargo run --release -- reclaim       # dry run: what's safe to free (deletes nothing)
cargo run --release -- reclaim --apply       # free regenerable caches (asks first)
```

## What it reports

1. **Where the space is** — the largest children of the scanned root.
2. **Reclaimable buckets** — known caches (Xcode, npm/yarn/pnpm/cargo/go,
   CocoaPods, Trash, …), always sized.
3. **Leak signatures** — anomalies above a baseline, each with owner, root
   cause, prevention, and a safe reclaim command (printed, never auto-run).

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
- [ ] Baseline learning — compare against a community median per directory,
      not just a static ceiling.
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
