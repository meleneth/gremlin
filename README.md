# Gremlin

Gremlin is a local-first file database, checksum, audit, and transfer-planning tool. Its main job is to make local and remote file browsing, mirroring, verification, and resume safer when networks, disks, or long copies fail. It tracks file history while preserving separate ideas of content identity, filename identity, and location identity.

This project is heavily vibe-coded with Codex using GPT-5.

The current version is still intentionally conservative: a single Rust CLI crate, a Tokio runtime baseline, a local SQLite database, append-only job events, projected query tables, stat-only scanning, file hashing, JSONL worker/import seams, persisted transfer plans, a local hash-checked copy runner, and a Ratatui TUI.

## Architecture Rule

The TUI may drive jobs, but it must not contain scan/hash/copy logic directly. File work is represented as commands and jobs. Jobs emit events. Events are persisted as durable history. The database projects current query state from those events and command results.

## Commands

```bash
gremlin /archive/photos
gremlin nas01:
gremlin --no-tui nas01:

gremlin init --db ./gremlin.db
gremlin config init --default-db ./gremlin.db --machine-label workstation

gremlin scan PATH --db ./gremlin.db
gremlin hash PATH --db ./gremlin.db
gremlin hash PATH --all --db ./gremlin.db
gremlin verify PATH --db ./gremlin.db
gremlin verify PATH --accept --db ./gremlin.db
gremlin --json status PATH --db ./gremlin.db

gremlin worker hash PATH --jsonl
gremlin worker hash PATH --jsonl --out checksums.jsonl

gremlin import-events checksums.jsonl --db ./gremlin.db
gremlin import-events checksums.jsonl --target nas01:/srv/archive --db ./gremlin.db
gremlin import-events checksums.jsonl --target nas01: --db ./gremlin.db
gremlin import-manifest checksums.sfv --db ./gremlin.db
gremlin import-manifest files.par2 --db ./gremlin.db

gremlin events --db ./gremlin.db
gremlin files --db ./gremlin.db
gremlin jobs --db ./gremlin.db
gremlin job create scan PATH --db ./gremlin.db
gremlin job create hash PATH --db ./gremlin.db
gremlin job show JOB_ID --db ./gremlin.db
gremlin job run JOB_ID --db ./gremlin.db
gremlin target inspect TARGET
gremlin target add TARGET --db ./gremlin.db
gremlin target add nas01: --db ./gremlin.db
gremlin target ls nas01: --db ./gremlin.db
gremlin target ls nas01: --path folder --db ./gremlin.db
gremlin status TARGET --db ./gremlin.db
gremlin transfer plan SOURCE DEST --db ./gremlin.db
gremlin transfer list --db ./gremlin.db
gremlin transfer show PLAN_ID --db ./gremlin.db
gremlin transfer run PLAN_ID --db ./gremlin.db
gremlin transfer run PLAN_ID --paranoid --db ./gremlin.db
gremlin tui --db ./gremlin.db
```

`--db` is a global override and may appear before or after a subcommand. If it is omitted, Gremlin checks `GREMLIN_DB`, then `default_db` in the config file. Positional target flows open the TUI by default; use `--no-tui` when you want only the command-line registration/status output.

For the smooth target flow, Gremlin can also auto-create a default database at:

```text
$XDG_DATA_HOME/gremlin/gremlin.db
~/.local/share/gremlin/gremlin.db
```

Config is loaded from `--config PATH`, then `GREMLIN_CONFIG`, then the default XDG-style path:

```text
$XDG_CONFIG_HOME/gremlin/config.json
~/.config/gremlin/config.json
```

Example config:

```json
{
  "default_db": "./gremlin.db",
  "machine_label": "workstation",
  "jobs_limit": 200
}
```

CLI overrides:

```bash
gremlin --db ./other.db files
gremlin --config ./gremlin.config.json jobs
gremlin --no-config --db ./scratch.db init
gremlin --machine-label laptop scan ~/archive
```

`gremlin TARGET` classifies a target, prepares/reuses the matching root, then opens the TUI unless `--no-tui` is supplied. For local directories it also runs a lightweight stat scan and prints status plus new/changed/missing highlights. For SSH-like and URL targets it registers metadata and prints status/hints without attempting remote execution.

`scan` walks a directory tree and records stat-level path observations. It reports new, changed, and missing paths. Missing paths are report-only in v0; no deletion or missing projection is performed.

Roots maintain `current_size_bytes`, the projected total size of currently indexed `present` file observations for that root.

`hash` walks a directory tree, computes BLAKE3 and SHA-256 for files that look new or changed from stat data, stores content objects, updates path observations, and persists hash events. Use `--all` to hash every regular file.

`verify` re-hashes current files and compares them to the latest stored per-path hashes. It reports `ok`, `changed`, `new`, `missing`, and `error`. By default it records history only; `--accept` promotes changed and new hashes into projected current truth.

`worker hash --jsonl` does not require a database. It emits JSONL events suitable for manual or future automated remote execution over SSH.

`import-events` reads JSONL events, preserves imported history in `job_events`, and creates checksum collection entries for completed hash events. With `--target TARGET`, completed hash events are also projected into that target root as current file observations and content objects. This is the current bridge for remote hashes: run or collect worker JSONL elsewhere, then import it into `nas01:/path` or `nas01:`.

`import-manifest` reads SFV/CFV-style CRC manifests and PAR2 file-description packets into checksum collections. PAR2 parity repair/verification and CRC verification against files are not implemented yet.

`job create` records an intended scan or hash job without executing file work. This is the same seam used by the TUI: UI actions create jobs, start them through the job runner, display projected progress, and can request cooperative cancellation between files.

In the Lospec500-themed TUI, Space marks/unmarks the selected file in a persisted default selection set for the current root. Press `s` or `h` to start local scan/hash jobs for a root, `c` to request cancellation, `v` to rotate file columns, `t` on a source root, move to a destination root, and press Enter to create a dry-run transfer plan from those marks. Esc cancels the destination selection.

`transfer plan SOURCE DEST` reads the source root's default TUI selection set, compares those marked paths against the destination root's current indexed observations, stores a durable transfer plan, records a `transfer_plan` job with append-only events, and prints a dry-run summary. It never copies or overwrites files. Initial actions are `copy`, `skip`, `verify_needed`, `conflict`, and `unavailable`.

`transfer list` shows recent dry-run plans. `transfer show PLAN_ID` prints the plan summary and capped file entries; use `--action copy`, `--action conflict`, or another action name to filter entries.

`transfer run PLAN_ID` is the conservative copy runner. It only executes plan entries whose action is `copy`, creates parent directories, refuses overwrites, compares copied bytes to the planned source content hash when one exists, records a `transfer_copy` job with per-file events, and writes the resulting content id onto the destination observation. Local-to-local copies hash the source stream while copying. SSH-to-local copies use `scp` into a temporary local file, hash it, then rename it into place. Local-to-SSH copies hash the local source first, verify the remote destination does not exist with `ssh`, then copy with `scp`. Remote-to-remote copies are not implemented. `--paranoid` is currently local-only; it fsyncs the file and parent directory before hashing the destination.

`target inspect` classifies obvious target forms without touching the database:

```bash
gremlin target inspect /archive/photos
gremlin target inspect file:///archive/photos
gremlin target inspect nas01:/mnt/archive
gremlin target inspect https://example.invalid/listing.json
```

Use `--kind local-path|file-url|ssh|url` only when you want to force interpretation. `target add` creates or reuses the matching machine/root record, and `status TARGET` gives a fast projected summary when that root is already known. SSH targets may be written as `host:/path` or `host:`; `host:` means the login default directory and is stored as `~`.

`target ls TARGET` lists cached child directories and files for a known root without touching the filesystem or network. Use `--path DIR` to list a cached subdirectory. This is currently backed by projected file observations, so it becomes useful after local scans/hashes or target-aware worker imports.

Most scan/hash/verify commands print a compact summary plus capped highlights. Use `--details` and `--limit N` to control result detail. `--json` is available for `status`, `scan`, `hash`, and `verify`.

## Development Notes

Future seams deliberately left open:

- SSH remote scan/hash dispatch: run `gremlin worker hash ... --jsonl --out ...` remotely, stream progress, and import results without manual file shuffling.
- Remote browsing: cache directory observations, let `host:` start at the default remote location, navigate from there, and promote browsed directories into tracked roots.
- Manifest reconciliation: use imported SFV/CFV/PAR2 checksum collections as verification baselines where possible.
- SMB path mapping: add machine/root mapping without changing content identity.
- Transfer planning/copying: persisted dry-run root-to-root plans, job events, CLI inspection, streamed hash-checked local copy execution, and optional paranoid readback exist for TUI selections; next slices should add richer TUI plan browsing, checksum collection comparisons, and resumable copy checkpoints.
- Seamless resume: make interrupted remote browsing, hashing, importing, and future copy jobs restart from durable job/event state instead of requiring manual cleanup.
- Metadata extractors: add new job kinds and events rather than expanding scan/hash responsibilities.
- Richer TUI job control: the TUI can start local jobs now; future slices should add progress, cancellation states, filtering, and async remote supervision without putting scan/hash/copy logic in TUI code.

## Known v0 Limits

- Path storage uses UTF-8 lossy display strings; raw non-UTF-8 Unix path support should be added later.
- Import preserves evidence and checksum entries. Target-aware worker imports can update projected root state for completed hash events, but full reconciliation from arbitrary checksum collections is not implemented.
- No deletion, daemon, remote SSH scan/hash dispatch, or metadata extraction is implemented. Transfer execution supports local-to-local and one-sided SSH copies through `ssh`/`scp`; remote-to-remote and paranoid SSH readback are not implemented.
