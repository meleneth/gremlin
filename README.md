# Gremlin

Gremlin is a local-first file database, checksum, audit, and transfer-planning tool. Its main job is to make local and remote file browsing, mirroring, verification, and resume safer when networks, disks, or long copies fail. It tracks file history while preserving separate ideas of content identity, filename identity, and location identity.

This project is heavily vibe-coded with Codex using GPT-5.

This first slice is intentionally small: a single Rust CLI crate, a Tokio runtime baseline, a local SQLite database, append-only job events, projected query tables, stat-only scanning, file hashing, JSONL worker/import seams, and a Ratatui TUI.

## Architecture Rule

The TUI may drive jobs, but it must not contain scan/hash/copy logic directly. File work is represented as commands and jobs. Jobs emit events. Events are persisted as durable history. The database projects current query state from those events and command results.

## Commands

```bash
gremlin /archive/photos

gremlin init --db ./gremlin.db
gremlin config init --default-db ./gremlin.db --machine-label workstation

gremlin scan PATH --db ./gremlin.db
gremlin hash PATH --db ./gremlin.db
gremlin hash PATH --all --db ./gremlin.db
gremlin verify PATH --db ./gremlin.db
gremlin verify PATH --accept --db ./gremlin.db

gremlin worker hash PATH --jsonl
gremlin worker hash PATH --jsonl --out checksums.jsonl

gremlin import-events checksums.jsonl --db ./gremlin.db
gremlin import-manifest checksums.sfv --db ./gremlin.db

gremlin events --db ./gremlin.db
gremlin files --db ./gremlin.db
gremlin jobs --db ./gremlin.db
gremlin job create scan PATH --db ./gremlin.db
gremlin job create hash PATH --db ./gremlin.db
gremlin job show JOB_ID --db ./gremlin.db
gremlin job run JOB_ID --db ./gremlin.db
gremlin target inspect TARGET
gremlin target add TARGET --db ./gremlin.db
gremlin status TARGET --db ./gremlin.db
gremlin transfer plan SOURCE DEST --db ./gremlin.db
gremlin transfer list --db ./gremlin.db
gremlin transfer show PLAN_ID --db ./gremlin.db
gremlin tui --db ./gremlin.db
```

`--db` is a global override and may appear before or after a subcommand. If it is omitted, Gremlin checks `GREMLIN_DB`, then `default_db` in the config file.

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

`gremlin TARGET` classifies a target. For local directories it creates/reuses the database and root, runs a lightweight stat scan, and prints status plus new/changed/missing highlights. For SSH-like and URL targets it registers metadata and prints status/hints without attempting remote execution.

`scan` walks a directory tree and records stat-level path observations. It reports new, changed, and missing paths. Missing paths are report-only in v0; no deletion or missing projection is performed.

Roots maintain `current_size_bytes`, the projected total size of currently indexed `present` file observations for that root.

`hash` walks a directory tree, computes BLAKE3 and SHA-256 for files that look new or changed from stat data, stores content objects, updates path observations, and persists hash events. Use `--all` to hash every regular file.

`verify` re-hashes current files and compares them to the latest stored per-path hashes. It reports `ok`, `changed`, `new`, `missing`, and `error`. By default it records history only; `--accept` promotes changed and new hashes into projected current truth.

`worker hash --jsonl` does not require a database. It emits JSONL events suitable for future remote execution over SSH.

`import-events` reads JSONL events, preserves imported history in `job_events`, and creates checksum collection entries for completed hash events.

`import-manifest` reads SFV/CFV-style CRC manifests and PAR2 file-description packets into checksum collections. PAR2 parity repair/verification is not implemented yet.

`job create` records an intended scan or hash job without executing file work. This is the same seam used by the TUI: UI actions create jobs, start them through the job runner, display projected progress, and can request cooperative cancellation between files.

In the TUI, Space marks/unmarks the selected file in a persisted default selection set for the current root. Press `t` on a source root, move to a destination root, and press Enter to create a dry-run transfer plan from those marks. Esc cancels the destination selection.

`transfer plan SOURCE DEST` reads the source root's default TUI selection set, compares those marked paths against the destination root's current indexed observations, stores a durable transfer plan, and prints a dry-run summary. It never copies or overwrites files. Initial actions are `copy`, `skip`, `verify_needed`, `conflict`, and `unavailable`.

`transfer list` shows recent dry-run plans. `transfer show PLAN_ID` prints the plan summary and capped file entries; use `--action copy`, `--action conflict`, or another action name to filter entries.

`target inspect` classifies obvious target forms without touching the database:

```bash
gremlin target inspect /archive/photos
gremlin target inspect file:///archive/photos
gremlin target inspect nas01:/mnt/archive
gremlin target inspect https://example.invalid/listing.json
```

Use `--kind local-path|file-url|ssh|url` only when you want to force interpretation. `target add` creates or reuses the matching machine/root record, and `status TARGET` gives a fast projected summary when that root is already known.

Most scan/hash/verify commands print a compact summary plus capped highlights. Use `--details` and `--limit N` to control result detail.

## Development Notes

Future seams deliberately left open:

- SSH remote dispatch: run `gremlin worker hash ... --jsonl --out ...` remotely, then copy JSONL back for import.
- Manifest imports: add SFV/CFV checksum manifests and PAR2 file-list extraction as checksum collection sources.
- SMB path mapping: add machine/root mapping without changing content identity.
- Transfer planning: persisted dry-run root-to-root plans and CLI inspection exist for TUI selections; next slices should add richer TUI plan browsing, checksum collection comparisons, and transfer jobs.
- Seamless resume: make interrupted remote browsing, hashing, importing, and future copy jobs restart from durable job/event state instead of requiring manual cleanup.
- Metadata extractors: add new job kinds and events rather than expanding scan/hash responsibilities.
- Richer TUI job control: the TUI can start local jobs now; future slices should add progress, cancellation states, filtering, and async remote supervision without putting scan/hash/copy logic in TUI code.

## Known v0 Limits

- Path storage uses UTF-8 lossy display strings; raw non-UTF-8 Unix path support should be added later.
- Import preserves evidence and checksum entries but does not perform full reconciliation.
- No deletion, transfer execution, daemon, remote SSH dispatch, or metadata extraction is implemented.
