# Gremlin TODO

## Try-it-now UX

- Make default command output calmer and more polished after repeated scans.
- Add clearer first-run messaging around the default XDG database path.
- Make the default `gremlin TARGET` flow explain what it knows, what it checked, and what actions are available next.

## TUI

- Replace inline decision prompts with modal popups for import mode, transfer confirmation/review, scoped jobs, retargeting, and destructive confirmations.
- Keep normal navigation centered on the root browser and files browser, with predictable Tab movement and decision modals only when a flow needs a choice.
- Add richer job/event payload detail beyond the current one-line preview.
- Show detailed import progress in the Details pane: current path, processed files/directories, queued files/directories, and whether progress is remote discovery or local indexing.
- Keep the Jobs pane to one row per queued/active/recent job; use the activity log for timing/event sequence details.
- Refresh/reconcile the Files pane at the currently browsed directory level so indexed rows, new rows, deletions, and metadata changes can be shown together.
- Improve layout for narrow terminals and long paths.

## Scan, Hash, Verify

- Keep missing files report-only until the observation history model is richer.
- Add explicit hash baseline selection for verify.
- Decide whether imported checksum collections can become verify baselines.
- Add PAR2 parity repair/verification; file-list import exists.
- Add richer changed reporting: size-only, mtime-only, hash mismatch.

## Data Model

- Add observation history so missing/new/changed evidence is queryable without overwriting projected state.
- Clarify naming around `roots.current_size_bytes`; today it is the projected total of currently indexed `present` observations.
- Add schema versioning/migrations instead of ad hoc `ALTER TABLE` checks.
- Preserve non-UTF-8 Unix paths instead of storing only lossy display strings.
- Add indexes for common root/path/job queries as data volume grows.

## Remote And Imports

- Build remote dispatch and progress streaming on Tokio rather than adding a separate sync orchestration path.
- Improve native SSH hash dispatch with streamed progress, better fallback detection, and resumable state.
- Implement SSH host-side chunk hashing that stores MD5 chunk evidence without copying file bytes locally first.
- Preserve remote hash/import job identity while also projecting target-aware imports into local import jobs.
- Treat SFV, CFV, PAR2, and worker JSONL as manifest/checksum collection sources.
- Add SMB path mapping and target normalization.
- Improve import reconciliation from checksum collections into projected state.
- Add safer handling for partial imports and duplicate event streams.
- Track resumable worker/import state so interrupted remote hash jobs can continue without starting over.
- Design remote browse/status around cached directory observations so flaky remote access does not make the UI useless.
- Add richer TUI cached directory navigation for persisted SSH roots, not only temporary live browse roots.
- Add explicit refresh/re-import controls for promoted remote roots.

## Transfer Planning

- Add compare flow between two roots or checksum collections.
- Add more transfer plan filters and output formats after the copy runner requirements settle.
- Add explicit TUI/CLI resume status for persisted transfer chunk checkpoints.
- Use stored path-observation chunk hashes to resume transfer plans even when copy checkpoints are missing.
- Make resume distinguish size-only skips from hash-verified skips.
- Add remote-to-remote transfer execution after one-sided SSH copies settle.
- Add SSH paranoid readback/hash verification after remote writes.
- Replace shell `ssh dd` transfer execution with owned SSH streams so SSH copies can emit smoother live byte/rate progress and avoid per-chunk process startup.

## Metadata Extractors

- Add extractor job kinds and events without expanding scan/hash responsibilities.
- Start with simple media/document metadata once the file evidence model stabilizes.
- Keep extractor failures recoverable job events.

## Correctness Caveats

- Incremental hashing relies on size/mtime and can miss same-size timestamp-preserved edits; use `hash --all` for full rebuilds.
- Verify reads bytes and can prove current content against stored hashes, but missing remains non-destructive.
- SSH browse, one-sided copy, fast stat import, and native remote SHA-256 hash import exist; remote-to-remote copy and streamed remote supervision are not implemented yet.
- Resume is not implemented yet; current jobs are durable records, but scan/hash execution still runs in one process.
