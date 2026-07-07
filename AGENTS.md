# Gremlin Agent Notes

## Git workflow

- Start each work session by checking repository state with `git status`.
- If a remote is configured, pull before making changes:
  - Prefer `git pull --ff-only`.
  - If the pull cannot fast-forward, stop and ask before merging or rebasing.
- Do not overwrite or revert user changes unless explicitly asked.
- Commit completed work in focused, reviewable slices.
- When work is done done, push the finished branch to the configured remote.
- If no remote is configured, say so clearly and leave the branch clean with local commits.

## Project basics

- Rust CLI project using SQLite through `rusqlite`.
- Keep file work out of the TUI. The TUI may create or view jobs, but it must not scan, hash, or verify files directly.
- Preserve append-only job events as evidence; projected tables represent current query state.
- Do not add deletion or transfer behavior unless explicitly requested.
- Run `cargo fmt`, `cargo clippy`, and `cargo test` before finalizing code changes.
