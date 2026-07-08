# Gremlin TODO

## Try-it-now UX

- Make default command output calmer and more polished after repeated scans.
- Add a command to preview hash candidates before running incremental hashing.
- Add clearer first-run messaging around the default XDG database path.
- Make the default `gremlin TARGET` flow explain what it knows, what it checked, and what actions are available next.

## TUI

- Add richer job/event payload detail beyond the current one-line preview.
- Add root filtering/search once large root lists are realistic.
- Add job progress and cancellation states for TUI-started jobs.
- Improve layout for narrow terminals and long paths.

## Scan, Hash, Verify

- Keep missing files report-only until the observation history model is richer.
- Add a way to accept a previous verify job after review, not only `verify --accept`.
- Add explicit hash baseline selection for verify.
- Decide whether imported checksum collections can become verify baselines.
- Add CRC verification against imported SFV/CFV checksum manifest entries.
- Add PAR2 parity repair/verification; file-list import exists.
- Add richer changed reporting: size-only, mtime-only, hash mismatch.
- Add a compact integrity summary per root.

## Data Model

- Add observation history so missing/new/changed evidence is queryable without overwriting projected state.
- Clarify naming around `roots.current_size_bytes`; today it is the projected total of currently indexed `present` observations.
- Add schema versioning/migrations instead of ad hoc `ALTER TABLE` checks.
- Preserve non-UTF-8 Unix paths instead of storing only lossy display strings.
- Add indexes for common root/path/job queries as data volume grows.

## Remote And Imports

- Build remote dispatch and progress streaming on Tokio rather than adding a separate sync orchestration path.
- Implement SSH dispatch for `worker hash --jsonl`.
- Preserve remote worker job identity while also projecting target-aware imports into local import jobs.
- Treat SFV, CFV, PAR2, and worker JSONL as manifest/checksum collection sources.
- Add SMB path mapping and target normalization.
- Improve import reconciliation from checksum collections into projected state.
- Add safer handling for partial imports and duplicate event streams.
- Track resumable worker/import state so interrupted remote hash jobs can continue without starting over.
- Design remote browse/status around cached directory observations so flaky remote access does not make the UI useless.
- Add TUI cached directory navigation for SSH roots, starting at the default `host:` location.
- Add a TUI flow to promote a browsed remote directory into a tracked root.

## Transfer Planning

- Improve TUI transfer plan display beyond the latest summary line.
- Add compare flow between two roots or checksum collections.
- Add more transfer plan filters and output formats after the copy runner requirements settle.
- Model copy chunks or per-file transfer checkpoints beyond the current whole-file copy runner.
- Make resume distinguish size-only skips from hash-verified skips.
- Add remote-to-remote transfer execution after one-sided SSH copies settle.
- Add SSH paranoid readback/hash verification after remote writes.
- Replace `scp` transfer execution with owned streams so SSH copies can emit live byte/rate progress.
- Add TUI controls to run a transfer plan and inspect copy progress.

## Metadata Extractors

- Add extractor job kinds and events without expanding scan/hash responsibilities.
- Start with simple media/document metadata once the file evidence model stabilizes.
- Keep extractor failures recoverable job events.

## Correctness Caveats

- Incremental hashing relies on size/mtime and can miss same-size timestamp-preserved edits; use `hash --all` for full rebuilds.
- Verify reads bytes and can prove current content against stored hashes, but missing remains non-destructive.
- Remote targets are registered and summarized only; no SSH execution exists yet.
- Resume is not implemented yet; current jobs are durable records, but scan/hash execution still runs in one process.
