# Gremlin

Gremlin is a local-first file database, checksum, audit, and transfer-planning tool. It tracks evidence about files while preserving separate ideas of content identity, filename identity, and location identity.

This first slice is intentionally small: a single Rust CLI crate, a local SQLite database, append-only job events, projected query tables, stat-only scanning, file hashing, JSONL worker/import seams, and a read-only Ratatui TUI.

## Architecture Rule

The TUI never performs file work directly. File work is represented as commands and jobs. Jobs emit events. Events are persisted as evidence. The database projects current query state from those events and command results.

## Commands

```bash
gremlin init --db ./gremlin.db

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
gremlin tui --db ./gremlin.db
```

`scan` walks a directory tree and records stat-level path observations. It does not hash file contents.

`hash` walks a directory tree, computes BLAKE3 and SHA-256, stores content objects, updates path observations, and persists hash events.

`worker hash --jsonl` does not require a database. It emits JSONL events suitable for future remote execution over SSH.

`import-events` reads JSONL events, preserves imported evidence in `job_events`, and creates checksum collection entries for completed hash events.

`job create` records an intended scan or hash job without executing file work. This is the same seam used by the TUI: UI actions enqueue jobs, while workers execute jobs and emit evidence later.

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
