# Gremlin

Gremlin is a local-first file database, checksum, audit, and transfer-planning tool. It tracks evidence about files while preserving separate ideas of content identity, filename identity, and location identity.

This first slice is intentionally small: a single Rust CLI crate, a local SQLite database, append-only job events, projected query tables, stat-only scanning, file hashing, JSONL worker/import seams, and a read-only Ratatui TUI.

## Architecture Rule

The TUI never performs file work directly. File work is represented as commands and jobs. Jobs emit events. Events are persisted as evidence. The database projects current query state from those events and command results.

## Commands

```bash
gremlin init --db ./gremlin.db
gremlin config init --default-db ./gremlin.db --machine-label workstation

gremlin scan PATH --db ./gremlin.db
gremlin hash PATH --db ./gremlin.db

gremlin worker hash PATH --jsonl
gremlin worker hash PATH --jsonl --out checksums.jsonl

gremlin import-events checksums.jsonl --db ./gremlin.db

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
gremlin tui --db ./gremlin.db
```

`--db` is a global override and may appear before or after a subcommand. If it is omitted, Gremlin checks `GREMLIN_DB`, then `default_db` in the config file.

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

`scan` walks a directory tree and records stat-level path observations. It does not hash file contents.

`hash` walks a directory tree, computes BLAKE3 and SHA-256, stores content objects, updates path observations, and persists hash events.

`worker hash --jsonl` does not require a database. It emits JSONL events suitable for future remote execution over SSH.

`import-events` reads JSONL events, preserves imported evidence in `job_events`, and creates checksum collection entries for completed hash events.

`job create` records an intended scan or hash job without executing file work. This is the same seam used by the TUI: UI actions enqueue jobs, while `job run` executes a queued job and emits evidence later.

`target inspect` classifies obvious target forms without touching the database:

```bash
gremlin target inspect /archive/photos
gremlin target inspect file:///archive/photos
gremlin target inspect nas01:/mnt/archive
gremlin target inspect https://example.invalid/listing.json
```

Use `--kind local-path|file-url|ssh|url` only when you want to force interpretation. `target add` creates or reuses the matching machine/root record, and `status TARGET` gives a fast projected summary when that root is already known.

## Development Notes

Future seams deliberately left open:

- SSH remote dispatch: run `gremlin worker hash ... --jsonl --out ...` remotely, then copy JSONL back for import.
- SMB path mapping: add machine/root mapping without changing content identity.
- Transfer planning: compare projected observations and checksum collections before adding transfer jobs.
- Metadata extractors: add new job kinds and events rather than expanding scan/hash responsibilities.
- Richer TUI job control: the TUI can enqueue jobs now; future slices should add job execution, cancellation states, and filtering without making the TUI scan or hash files directly.

## Known v0 Limits

- Path storage uses UTF-8 lossy display strings; raw non-UTF-8 Unix path support should be added later.
- Import preserves evidence and checksum entries but does not perform full reconciliation.
- No deletion, transfer, daemon, remote SSH dispatch, or metadata extraction is implemented.
