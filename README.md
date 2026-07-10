# Gremlin

Gremlin is a local-first file database, checksum, audit, and transfer-planning tool. Its main job is to make local and remote file browsing, mirroring, verification, and resume safer when networks, disks, or long copies fail. It tracks file history while preserving separate ideas of content identity, filename identity, and location identity.

This project is heavily vibe-coded with Codex using GPT-5.

The current version is still intentionally conservative: a single Rust CLI crate, a Tokio runtime baseline, a local SQLite database, append-only job events, projected query tables, stat-only scanning, file hashing, JSONL worker/import seams, persisted transfer plans, TUI-queued transfer execution, a hash-checked local and one-sided SSH copy runner, resumable SSH chunk checkpoints, and a Ratatui TUI.

## Build and Install

Gremlin is a standard Rust binary crate. From the repository root:

```bash
cargo run -- --help
cargo run -- tui

cargo build
cargo build --release

cargo install --path .
gremlin --help
```

`cargo install --path .` installs the `gremlin` binary into Cargo's bin directory, usually `~/.cargo/bin`. Make sure that directory is on `PATH` if `gremlin` is not found after install.

For a one-off release binary without installing it:

```bash
cargo build --release
./target/release/gremlin --help
```

Manual SSH checksum imports can still use `gremlin worker hash --jsonl` if a compatible `gremlin` binary is available on the remote host. The TUI's SSH hash import path uses standard remote shell tools instead.

## Development Checks

Before finalizing code changes:

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
bundle exec rspec
```

The RSpec suite in `spec/` is a black-box CLI integration suite. It builds or uses
the `gremlin` binary, creates disposable fixture directories and database files,
removes the test database before each example, and verifies behavior only through
command exit status, stdout/stderr, JSON output, and filesystem effects. To use a
specific binary:

```bash
GREMLIN_BIN=./target/release/gremlin bundle exec rspec
```

Coverage uses `cargo-llvm-cov`:

```bash
cargo install cargo-llvm-cov
cargo llvm-cov --all-targets --all-features
cargo llvm-cov --all-targets --all-features --html
```

The HTML report is written to `target/llvm-cov/html/index.html`.

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
gremlin chunk-hash PATH --chunk-size-mib 64 --db ./gremlin.db
gremlin verify PATH --db ./gremlin.db
gremlin verify PATH --accept --db ./gremlin.db
gremlin verify-collection COLLECTION_ID TARGET --db ./gremlin.db
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
gremlin target remove TARGET --yes --db ./gremlin.db
gremlin target ls nas01: --db ./gremlin.db
gremlin target ls nas01: --path folder --db ./gremlin.db
gremlin status TARGET --db ./gremlin.db
gremlin transfer plan SOURCE DEST --db ./gremlin.db
gremlin transfer plan SOURCE DEST --all --db ./gremlin.db
gremlin transfer list --db ./gremlin.db
gremlin transfer show PLAN_ID --db ./gremlin.db
gremlin transfer run PLAN_ID --db ./gremlin.db
gremlin transfer run PLAN_ID --paranoid --db ./gremlin.db
gremlin tui --db ./gremlin.db
```

`--db` is a global override and may appear before or after a subcommand. If it is omitted, Gremlin checks `GREMLIN_DB`, then `default_db` in the config file. Positional target flows open the TUI by default; use `--no-tui` when you want only the command-line registration/status output. Passing `host:` or `host:/path` as a positional target starts from a temporary SSH browse target; it does not persist a root unless you explicitly import observations for that target or run `target add`.

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

`gremlin TARGET` classifies a target, prepares/reuses the matching root, then opens the TUI unless `--no-tui` is supplied. For local directories it also runs a lightweight stat scan and prints status plus new/changed/missing highlights. Before opening the TUI for an SSH target, Gremlin probes passwordless SSH with batch mode; if the key is not available, it can run `ssh-copy-id HOST` in the normal terminal so the standard SSH password/key prompt can install the key, then it retries the batch probe before continuing. For SSH-like and URL targets it registers metadata and prints status/hints without attempting remote execution when `--no-tui` is supplied.

`scan` walks a directory tree and records stat-level path observations. It reports new, changed, and missing paths. Missing paths are report-only in v0; no deletion or missing projection is performed.

Roots maintain `current_size_bytes`, the projected total size of currently indexed `present` file observations for that root.

`hash` walks a directory tree, computes BLAKE3 and SHA-256 for files that look new or changed from stat data, stores content objects, updates path observations, and persists hash events. Use `--all` to hash every regular file.

`chunk-hash` is explicit opt-in evidence collection for local roots. It computes MD5 chunks for each file, stores them on the current path observation, and does not run as part of normal scan/hash/copy. The default chunk size is 64 MiB; use `--chunk-size-mib` to change it. This is separate from transfer copy checkpoints: one-sided SSH copies also use 64 MiB MD5 chunks while copying, but those checkpoints are stored per transfer plan so interrupted copies can resume and identify the failed chunk.

`verify` re-hashes current files and compares them to the latest stored per-path hashes. It reports `ok`, `changed`, `new`, `missing`, and `error`. By default it records history only; `--accept` promotes changed and new hashes into projected current truth.

`verify-collection COLLECTION_ID TARGET` compares an imported checksum collection to a known root's current projected observations. It does not read or hash filesystem files; run `scan`, `hash --all`, SSH hash import, or a transfer first when the root needs fresher evidence. Results distinguish hash `ok`, `missing`, `size_mismatch`, `hash_mismatch`, `unverified` when the root lacks comparable hash evidence, `size_only` when only sizes can be compared, and extra root files that are not present in the collection.

`worker hash --jsonl` does not require a database. It emits JSONL events suitable for manual or future automated remote execution over SSH.

`import-events` reads JSONL events, preserves imported history in `job_events`, and creates checksum collection entries for completed hash events. With `--target TARGET`, completed hash events are also projected into that target root as current file observations and content objects. This is the current bridge for remote hashes: run or collect worker JSONL elsewhere, then import it into `nas01:/path` or `nas01:`.

`import-manifest` reads SFV/CFV-style CRC manifests and PAR2 file-description packets into checksum collections. PAR2 parity repair/verification and CRC verification against files are not implemented yet.

`job create` records an intended scan or hash job without executing file work. This is the same seam used by the TUI: UI actions create jobs, start them through the job runner, display projected progress, and can request cooperative cancellation between files.

In the TUI, Space marks/unmarks the selected file in a persisted default selection set for the current root. Directory marks include currently indexed descendant files. The Files pane supports `/` filtering across persisted and temporary browse lists, Esc clears the active filter, `u` refreshes the current temporary remote browse listing, and PageUp/PageDown jump through long lists. File evidence glyphs are shown in the Files pane legend: `◇` remote-only, `◌` indexed fast/stat evidence, `◆` hash evidence, `◉` available in a local indexed root, `!` live remote metadata changed from the indexed row, `×` indexed row missing from the live remote listing, and `▸` directory. When marks exist, `s`, `h`, and `v` ask whether to scan, hash, or verify the whole root or only marked paths; marked-directory scope covers the indexed file set, not yet an open-ended subtree discovery scan. Temporary SSH browse targets opened with `gremlin host:` appear as browse-only roots; Enter on a temporary directory navigates into it and Backspace returns toward the temporary root without persisting anything. Press `i` on a temporary SSH browse root to import the selected remote file or navigated directory: `n` persists only the root, `f` recursively imports fast stat observations, and `h` runs native SSH `find`/`sha256sum` hashing and imports the resulting SHA-256 checksum evidence. Imported temporary SSH roots are persisted as `host:/absolute/remote/path`, resolved from the path the user actually browsed to, not merely the original positional target.

The TUI's Gremlin command pane shows the active command hints, including modal choices such as import mode, retarget editing, delete confirmation, and scoped job choices. Press `s`, `h`, or `v` to start scan, hash, or verify jobs for a persisted root, `m` to compare the selected root against the newest attached or unattached checksum collection, `x` to remove a persisted root after a `y` confirmation, `c` to request cancellation, `f` to rotate file fields, `t` on a source root, move to a destination root, and press Enter to create a dry-run transfer plan from those marks. Esc cancels the destination selection. Created plans appear in the Plan pane with `copy`, `review`, `conflict`, and other actions visible next to the affected paths; `review` rows include collision counts so duplicate content or filename/size/date matches are visible before anything is copied. Collection comparisons also appear in the Plan pane with `ok`, `missing`, `size_mismatch`, `hash_mismatch`, `unverified`, `size_only`, and `extra` rows.

Press `p` on a root to load its most recent persisted transfer plan. Canceled, queued, and running transfer plans appear at the bottom of the Roots pane under `Resume` as `R` rows. Press Enter on a resume row to load that plan into the Plan pane, or press `r` on a resume row to load and queue it immediately. With the Plan pane focused, press `a` to accept the selected `review` row as `copy`, `d` to drop it as `skip`, `e` to type a new destination path and retarget it as `copy`, or `r` to queue the current plan's `copy` entries. The TUI runs only one transfer at a time; additional transfer plans are marked `queued` and start automatically in creation order when the active transfer finishes. The Activity Log shows durable job events and includes a target column for transfer source-to-destination display, so repeated rows with the same job id are events for the same job rather than parallel jobs.

During transfer runs, the Info frame shows the active transfer file, job and current-file progress bars, copied bytes, file counts, errors, transfer rate, and SSH chunk checkpoint state when available. Details stays focused on the selected root/file metadata and points to Info while a transfer is active. The Events pane follows the plan's source root while Plan is focused so durable progress events stay visible too. Pressing Ctrl-C or Ctrl-D in the TUI requests cancellation for active database jobs, marks active transfer plans canceled, restores the terminal, and exits immediately with code 130. Cooperative transfer cancellation stops between files; an immediate TUI exit can leave resumable per-plan chunk checkpoints for the next run of the same plan.

`transfer plan SOURCE DEST` reads the source root's default TUI selection set, compares those marked paths against the destination root's current indexed observations, stores a durable transfer plan, records a `transfer_plan` job with append-only events, and prints a dry-run summary. Use `--all` to plan every currently indexed `present` file in the source root without changing the TUI selection set. Planning never copies or overwrites files. Actions are `copy`, `review`, `skip`, `verify_needed`, `conflict`, and `unavailable`. `review` means the destination path may be empty, but the destination root already has matching content hash elsewhere or another file with the same filename/size/modified-time signature; use `transfer show PLAN_ID --action review` to inspect collision metadata before deciding what to copy.

`transfer list` shows recent dry-run plans. `transfer show PLAN_ID` prints the plan summary and capped file entries; use `--action copy`, `--action conflict`, or another action name to filter entries. `transfer decide PLAN_ID RELATIVE_PATH --decision accept` changes a `review` row into a runnable `copy` row; `--decision drop` changes it into `skip`; `--decision retarget --dest NEW/PATH` changes the destination path and makes the row runnable as `copy`. Other actions are not changed by this decision path.

`transfer run PLAN_ID` is the conservative copy runner. It only executes plan entries whose action is `copy`; `review`, `conflict`, `verify_needed`, and other non-copy entries stay untouched. By default, destination paths preserve the source-relative path under the destination root: copying `foo/some/file.png` from root `foo` to root `bar` writes `bar/some/file.png` unless that plan entry has been explicitly retargeted. The runner creates parent directories, refuses overwrites, preserves the source modified time where supported, compares copied bytes to the planned source content hash when one exists, records a `transfer_copy` job with per-file events, and writes the resulting content id and modified time onto the destination observation. Local-to-local copies hash the source stream while copying and emit byte progress with transfer rate; the TUI renders those job progress events as compact bars with KiB/s or MiB/s. Cancellation requests are honored between files and mark both the copy job and transfer plan as `canceled`.

One-sided SSH copies run in 64 MiB chunks and persist per-plan chunk checkpoints: SSH-to-local precomputes each remote chunk's MD5 before streaming it into a stable temporary local file, and local-to-SSH writes each chunk with `ssh dd` then verifies the remote chunk MD5 before continuing. SSH progress events name the confidence state explicitly, including `reused local checkpoint after MD5 verify`, `reused remote checkpoint after MD5 verify`, `checkpoint miss; fetched and MD5 verified remote chunk`, and `checkpoint miss; rewrote and MD5 verified remote chunk`. Re-running the same plan can skip checkpointed chunks after re-verifying the partial local file or remote chunk. Remote-to-remote copies are not implemented. `--paranoid` is currently local-only; it fsyncs the file and parent directory before hashing the destination.

`target inspect` classifies obvious target forms without touching the database:

```bash
gremlin target inspect /archive/photos
gremlin target inspect file:///archive/photos
gremlin target inspect nas01:/mnt/archive
gremlin target inspect https://example.invalid/listing.json
```

Use `--kind local-path|file-url|ssh|url` only when you want to force interpretation. `target add` creates or reuses the matching machine/root record, and `target remove TARGET` previews removal of the matching root's Gremlin database records; add `--yes` to actually remove the root, its indexed observations, selections, transfer plans, checksum collections, and root jobs/events. Root removal never deletes filesystem files. `status TARGET` gives a fast projected summary when that root is already known. SSH targets may be written as `host:/path` or `host:`; `host:` means the login default directory and is stored as `~`. Positional SSH targets are temporary until promoted with `target add` or populated through an import command.

`target ls TARGET` live-lists SSH directories with a bounded `ssh` probe before falling back to cached observations for a persisted root. Use `--path DIR` to list a child directory. Local and file URL targets are backed by projected file observations, so they become useful after local scans/hashes or target-aware worker imports.

Most scan/hash/verify commands print a compact summary plus capped highlights. Use `--details` and `--limit N` to control result detail. `--json` is available for `status`, `scan`, `hash`, and `verify`.

## Development Notes

Future seams deliberately left open:

- SSH remote scan/hash dispatch: TUI import can hash through native remote `find`/`sha256sum`; next steps are live progress streaming and resumable remote worker state.
- Remote browsing: live temporary SSH listings can be navigated in the TUI and imported as roots; next steps are richer cached directory observations and explicit refresh controls.
- Manifest reconciliation: checksum collections can now be compared to root observations by path, size, and comparable BLAKE3/SHA-256 hashes from the CLI and TUI; next steps are CRC/PAR2-specific verification and richer collection selection.
- SMB path mapping: add machine/root mapping without changing content identity.
- Transfer planning/copying: persisted dry-run root-to-root plans, job events, CLI inspection, TUI persisted-plan loading, queueing, resume rows, TUI plan browsing/run/review/retarget controls, detailed transfer progress, streamed hash-checked local copy execution, checkpointed chunk-verified one-sided SSH copies, optional local root chunk hashes, optional local paranoid readback, and checksum collection comparison exist for TUI selections; next slices should add richer queue controls and SSH resume summaries.
- Seamless resume: make interrupted remote browsing, hashing, importing, and future copy jobs restart from durable job/event state instead of requiring manual cleanup.
- Metadata extractors: add new job kinds and events rather than expanding scan/hash responsibilities.
- Richer TUI job control: the TUI can start local jobs and queue transfer runs now; future slices should add better job filtering, queue reordering/drop controls, clearer cancellation states, transfer chunk/resume summaries, and async remote supervision without putting scan/hash/copy logic in TUI code.

## Known v0 Limits

- Path storage uses UTF-8 lossy display strings; raw non-UTF-8 Unix path support should be added later.
- Import preserves evidence and checksum entries. Target-aware worker imports can update projected root state for completed hash events. `verify-collection` can compare imported collections against projected root state, but CRC/PAR2 repair and verification are not implemented.
- No daemon, remote-to-remote transfer, queued-transfer reordering, streamed SSH supervision, or metadata extraction is implemented. Transfer execution supports local-to-local and one-sided SSH copies through `ssh`; remote import supports fast stat observations and native SSH SHA-256 hashing.
