mod cli;
mod collections;
mod config;
mod crc32;
mod db;
mod error;
mod events;
mod fswork;
mod import;
mod remote_helper;
mod root_snapshot;
mod targets;
mod transfer;
mod tui;
mod util;

use anyhow::Context;
use clap::Parser;
use cli::{
    Cli, Commands, ConfigCommands, DbCommands, JobCommands, RootCommands, TargetCommands,
    TransferCommands, WorkerCommands,
};
use events::{EventEnvelope, EventKind, EventPayload};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use targets::{ParsedTarget, TargetKind};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config_ctx = config::load(cli.config.clone(), cli.no_config)?;
    let machine_label = config_ctx.machine_label(cli.machine_label.clone());
    let output = fswork::OutputOptions {
        details: cli.details,
        limit: cli.limit,
        quiet: false,
        json: cli.json,
    };

    match cli.command {
        None => {
            let Some(target) = cli.target.as_deref() else {
                if cli.no_tui {
                    anyhow::bail!("nothing to do: pass a command or target, or omit --no-tui");
                }
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_or_create(&db)?;
                db::init_schema(&conn)?;
                let open_root_provider = open_root_provider(db.clone(), machine_label.clone());
                tui::run_with_options(&conn, &db, machine_label, open_root_provider).await?;
                return Ok(());
            };
            let target_path = std::path::Path::new(target);
            if root_snapshot::looks_like_snapshot_file(target_path) {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_or_create(&db)?;
                db::init_schema(&conn)?;
                let result = root_snapshot::import_snapshot_file(&conn, target_path)?;
                println!("db:\t{}", db.display());
                println!("imported_root:\t{}\t{}", result.root_id, result.root_path);
                println!("files:\t{}", result.file_count);
                return Ok(());
            }
            let default_target = run_default_target(
                &config_ctx,
                cli.db.clone(),
                target,
                machine_label.as_deref(),
                output,
                !cli.no_tui,
            )?;
            if !cli.no_tui {
                let conn = db::open_existing(&default_target.db_path)?;
                let open_root_provider =
                    open_root_provider(default_target.db_path.clone(), machine_label.clone());
                tui::run_with_initial_browse(
                    &conn,
                    &default_target.db_path,
                    machine_label,
                    default_target.initial_browse,
                    open_root_provider,
                )
                .await?;
            }
        }
        Some(Commands::Init) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn =
                db::open_or_create(&db).with_context(|| format!("opening {}", db.display()))?;
            db::init_schema(&conn)?;
            println!("initialized {}", db.display());
        }
        Some(Commands::Scan { path }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            fswork::scan_to_db(&conn, &path, &db, machine_label.as_deref(), output)?;
        }
        Some(Commands::Hash { path, all }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            fswork::hash_to_db(&conn, &path, &db, machine_label.as_deref(), all, output)?;
        }
        Some(Commands::ChunkHash {
            target,
            kind,
            chunk_size_mib,
        }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            let parsed = targets::parse_target(&target, kind)?;
            let chunk_size_bytes = chunk_size_mib
                .checked_mul(1024 * 1024)
                .ok_or_else(|| anyhow::anyhow!("chunk size is too large"))?;
            match parsed.kind {
                TargetKind::LocalPath | TargetKind::FileUrl => {
                    let local_path = parsed.local_path().ok_or_else(|| {
                        anyhow::anyhow!("chunk-hash target is not local file-like")
                    })?;
                    fswork::chunk_hash_to_db(
                        &conn,
                        &local_path,
                        &db,
                        machine_label.as_deref(),
                        chunk_size_bytes,
                        output,
                    )?;
                }
                TargetKind::Ssh => {
                    remote_chunk_hash_to_db(
                        &conn,
                        &parsed,
                        machine_label.as_deref(),
                        chunk_size_bytes,
                        output,
                    )?;
                }
                TargetKind::Url => anyhow::bail!("chunk-hash does not support URL targets yet"),
            }
        }
        Some(Commands::Verify {
            target,
            accept,
            kind,
        }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            let parsed = targets::parse_target(&target, kind)?;
            if !matches!(parsed.kind, TargetKind::LocalPath | TargetKind::FileUrl) {
                anyhow::bail!("verify currently supports local path and file:// targets only");
            }
            let local_path = parsed
                .local_path()
                .ok_or_else(|| anyhow::anyhow!("verify target is not local file-like"))?;
            fswork::verify_to_db(
                &conn,
                &local_path,
                &db,
                machine_label.as_deref(),
                accept,
                output,
            )?;
        }
        Some(Commands::VerifyCollection {
            collection_id,
            target,
            kind,
        }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            let root = resolve_registered_root(&conn, &target, kind, machine_label.as_deref())?;
            let summary =
                collections::verify_collection_against_root(&conn, &collection_id, &root)?;
            print_collection_verify_summary(summary, output)?;
        }
        Some(Commands::Worker { command }) => match command {
            WorkerCommands::Hash { path, jsonl, out } => {
                if !jsonl {
                    anyhow::bail!("worker hash currently requires --jsonl");
                }
                fswork::worker_hash_jsonl(&path, out.as_deref())?;
            }
        },
        Some(Commands::ImportEvents {
            input,
            target,
            kind,
        }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            let import_target = match target {
                Some(target) => {
                    let parsed = targets::parse_target(&target, kind)?;
                    let (machine_id, root_path) =
                        resolve_target_identity(&conn, &parsed, machine_label.as_deref())?;
                    let root_id = db::ensure_root(&conn, &machine_id, &root_path)?;
                    Some(import::EventImportTarget {
                        machine_id,
                        root_id,
                        root_path,
                    })
                }
                None => None,
            };
            if let Some(import_target) = import_target.as_ref() {
                import::import_events_file_for_target(&conn, &input, Some(import_target))?;
            } else {
                import::import_events_file(&conn, &input)?;
            }
        }
        Some(Commands::ImportManifest { input }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            import::import_manifest_file(&conn, &input)?;
        }
        Some(Commands::Events) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            for row in db::recent_events(&conn, config_ctx.jobs_limit())? {
                println!(
                    "{} {} #{} {} {}",
                    row.created_at, row.job_id, row.sequence, row.event_kind, row.payload_json
                );
            }
        }
        Some(Commands::Files) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            for row in db::recent_files(&conn, config_ctx.jobs_limit())? {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    row.size_bytes,
                    row.status,
                    row.modified_at.unwrap_or_else(|| "-".to_string()),
                    row.content_id.unwrap_or_else(|| "-".to_string()),
                    row.relative_path
                );
            }
        }
        Some(Commands::Jobs) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            for row in db::recent_jobs(&conn, config_ctx.jobs_limit())? {
                println!(
                    "{}\t{}\t{}\t{}\t{}/{}\tskipped={}\terrors={}\t{}\t{}",
                    row.id,
                    row.kind,
                    row.status,
                    row.phase.unwrap_or_else(|| "-".to_string()),
                    row.files_done,
                    row.files_seen,
                    row.files_skipped,
                    row.errors,
                    row.created_at,
                    row.params_json.unwrap_or_else(|| "{}".to_string())
                );
            }
        }
        Some(Commands::Db { command }) => match command {
            DbCommands::Delete { yes } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                delete_database_file(&db, yes)?;
            }
        },
        Some(Commands::Job { command }) => match command {
            JobCommands::Create { kind, path } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let job_id =
                    db::queue_file_job(&conn, kind.as_str(), &path, machine_label.as_deref())?;
                println!("queued {} job {job_id}", kind.as_str());
            }
            JobCommands::Show { job_id } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let Some(job) = db::job_by_id(&conn, &job_id)? else {
                    anyhow::bail!("job not found: {job_id}");
                };
                println!("id:\t{}", job.id);
                println!("kind:\t{}", job.kind);
                println!("status:\t{}", job.status);
                println!(
                    "machine:\t{}",
                    job.machine_id.unwrap_or_else(|| "-".to_string())
                );
                println!("root:\t{}", job.root_id.unwrap_or_else(|| "-".to_string()));
                println!("created:\t{}", job.created_at);
                println!(
                    "started:\t{}",
                    job.started_at.unwrap_or_else(|| "-".to_string())
                );
                println!(
                    "completed:\t{}",
                    job.completed_at.unwrap_or_else(|| "-".to_string())
                );
                println!("phase:\t{}", job.phase.unwrap_or_else(|| "-".to_string()));
                println!(
                    "progress:\tseen={} done={} skipped={} errors={} total={} current={}",
                    job.files_seen,
                    job.files_done,
                    job.files_skipped,
                    job.errors,
                    job.files_total,
                    job.current_path.unwrap_or_else(|| "-".to_string())
                );
                println!("cancel_requested:\t{}", job.cancel_requested);
                println!(
                    "params:\t{}",
                    job.params_json.unwrap_or_else(|| "{}".to_string())
                );
                for event in db::events_for_job(&conn, &job_id)? {
                    println!(
                        "event:\t#{}\t{}\t{}\t{}",
                        event.sequence, event.created_at, event.event_kind, event.payload_json
                    );
                }
            }
            JobCommands::Run { job_id } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                fswork::run_queued_job(&conn, &job_id, &db, machine_label.as_deref(), output)?;
            }
        },
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Init {
                path,
                default_db,
                machine_label,
            } => {
                let config = config::GremlinConfig {
                    default_db,
                    machine_label,
                    jobs_limit: Some(200),
                };
                let written = config::write_default(path.or(cli.config.clone()), &config)?;
                println!("wrote {}", written.display());
            }
            ConfigCommands::Show { format: _ } => {
                println!("{}", serde_json::to_string_pretty(&config_ctx.config)?);
            }
            ConfigCommands::Path => {
                if let Some(path) = config_ctx.path {
                    println!("{}", path.display());
                } else if let Some(path) = config::default_config_path() {
                    println!("{}", path.display());
                }
            }
        },
        Some(Commands::Target { command }) => match command {
            TargetCommands::Inspect { target, kind } => {
                let parsed = targets::parse_target(&target, kind)?;
                println!("{}", serde_json::to_string_pretty(&parsed)?);
            }
            TargetCommands::Add {
                target,
                kind,
                label,
            } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let parsed = targets::parse_target(&target, kind)?;
                let (machine_id, root_path) =
                    resolve_target_identity(&conn, &parsed, machine_label.as_deref())?;
                let root_id = db::ensure_root(&conn, &machine_id, &root_path)?;
                if let Some(label) = label {
                    db::set_root_label(&conn, &root_id, &label)?;
                }
                println!(
                    "target {:?}\tmachine={}\troot={}\tpath={}",
                    parsed.kind, machine_id, root_id, root_path
                );
            }
            TargetCommands::Remove { target, kind, yes } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let parsed = targets::parse_target(&target, kind)?;
                let (machine_id, root_path) =
                    resolve_target_identity(&conn, &parsed, machine_label.as_deref())?;
                let root = db::find_root_by_machine_path(&conn, &machine_id, &root_path)?
                    .ok_or_else(|| anyhow::anyhow!("target is not a known root: {target}"))?;
                let summary = db::root_delete_summary(&conn, &root.id)?;
                if !yes {
                    print_root_delete_summary(&root, &summary);
                    println!("confirm:\trerun with --yes to remove this root from the database");
                    return Ok(());
                }
                let Some(summary) = db::delete_root(&conn, &root.id)? else {
                    anyhow::bail!("target is not a known root: {target}");
                };
                println!("removed:\t{}\t{}\t{}", root.id, root.path, root.machine_id);
                print_root_delete_counts(&summary);
                println!("note:\tno filesystem files were deleted");
            }
            TargetCommands::Ls { target, kind, path } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let parsed = targets::parse_target(&target, kind)?;
                let (machine_id, root_path) =
                    resolve_target_identity(&conn, &parsed, machine_label.as_deref())?;
                if parsed.kind == TargetKind::Ssh {
                    let scan_path = remote_child_path(&root_path, &path);
                    match fast_scan_ssh_directory(&parsed, &scan_path) {
                        Ok(entries) => {
                            if entries.is_empty() {
                                println!("empty:\t{path}");
                            }
                            for entry in entries {
                                print_fast_directory_entry(&entry);
                            }
                            return Ok(());
                        }
                        Err(err) => {
                            eprintln!("warning:\tSSH fastscan failed: {err}");
                        }
                    }
                }
                let root = db::find_root_by_machine_path(&conn, &machine_id, &root_path)?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "target is not a known root yet: {target}. Run `gremlin target add {target}` or import observations for it first"
                        )
                    })?;
                let entries = db::cached_directory_entries(&conn, &root.id, &path)?;
                if entries.is_empty() {
                    println!("empty:\t{path}");
                }
                for entry in entries {
                    print_cached_directory_entry(&entry);
                }
            }
        },
        Some(Commands::Root { command }) => match command {
            RootCommands::Export { target, kind } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let root = resolve_registered_root(&conn, &target, kind, machine_label.as_deref())?;
                let result = root_snapshot::export_root(&conn, &root)?;
                println!("exported_root:\t{}\t{}", root.id, root.path);
                println!("file:\t{}", result.path.display());
                println!("files:\t{}", result.file_count);
            }
            RootCommands::ExportSfv {
                target,
                kind,
                output,
            } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let root = resolve_registered_root(&conn, &target, kind, machine_label.as_deref())?;
                export_root_sfv(&conn, &root, output.as_deref())?;
            }
            RootCommands::Import { input } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_or_create(&db)?;
                db::init_schema(&conn)?;
                let result = root_snapshot::import_snapshot_file(&conn, &input)?;
                println!("imported_root:\t{}\t{}", result.root_id, result.root_path);
                println!("files:\t{}", result.file_count);
            }
        },
        Some(Commands::Transfer { command }) => match command {
            TransferCommands::List => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                for plan in db::recent_transfer_plans(&conn, output.limit as i64)? {
                    print_transfer_plan_row(&conn, &plan)?;
                }
            }
            TransferCommands::Plan {
                source,
                dest,
                all,
                source_kind,
                dest_kind,
            } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let source_root =
                    resolve_registered_root(&conn, &source, source_kind, machine_label.as_deref())?;
                let dest_root =
                    resolve_registered_root(&conn, &dest, dest_kind, machine_label.as_deref())?;
                let result = if all {
                    transfer::plan_all_files(&conn, &source_root, &dest_root)?
                } else {
                    transfer::plan_selected_files(&conn, &source_root, &dest_root)?
                };
                print_transfer_plan(&conn, &source_root, &dest_root, result, output)?;
            }
            TransferCommands::Show { plan_id, action } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let Some(plan) = db::transfer_plan_by_id(&conn, &plan_id)? else {
                    anyhow::bail!("transfer plan not found: {plan_id}");
                };
                print_transfer_plan_row(&conn, &plan)?;
                let entries =
                    db::transfer_plan_entries_filtered(&conn, &plan.id, action.as_deref())?;
                for entry in entries.into_iter().take(output.limit) {
                    print_transfer_entry(&entry);
                }
            }
            TransferCommands::Decide {
                plan_id,
                relative_path,
                decision,
                dest,
            } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let Some(plan) = db::transfer_plan_by_id(&conn, &plan_id)? else {
                    anyhow::bail!("transfer plan not found: {plan_id}");
                };
                let changed = if matches!(decision, cli::TransferDecision::Retarget) {
                    let Some(dest) = dest.as_deref() else {
                        anyhow::bail!("--dest is required with --decision retarget");
                    };
                    db::retarget_review_transfer_plan_entry(&conn, &plan.id, &relative_path, dest)?
                } else {
                    if dest.is_some() {
                        anyhow::bail!("--dest is only valid with --decision retarget");
                    }
                    db::decide_review_transfer_plan_entry(
                        &conn,
                        &plan.id,
                        &relative_path,
                        decision.action(),
                        decision.reason(),
                        serde_json::json!({
                            "decision": decision.as_str(),
                            "decided_at": util::now_rfc3339(),
                        }),
                    )?
                };
                if !changed {
                    anyhow::bail!(
                        "no review entry found for {relative_path} in transfer plan {plan_id}"
                    );
                }
                if let Some(dest) = dest {
                    println!(
                        "decision:\t{}\t{}\t{}\t{}",
                        decision.as_str(),
                        decision.action(),
                        relative_path,
                        dest
                    );
                } else {
                    println!(
                        "decision:\t{}\t{}\t{}",
                        decision.as_str(),
                        decision.action(),
                        relative_path
                    );
                }
            }
            TransferCommands::Run { plan_id, paranoid } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let result = transfer::run_transfer_plan(&conn, &plan_id, paranoid)?;
                println!("transfer_run:\t{}", result.job_id);
                println!("plan:\t{}", result.plan_id);
                println!(
                    "copied:\t{}\t{}",
                    result.copied,
                    util::human_size(result.bytes_copied)
                );
                println!("skipped:\t{}", result.skipped);
                println!("errors:\t{}", result.errors);
                println!("canceled:\t{}", result.canceled);
            }
        },
        Some(Commands::Status { target, kind }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            let parsed = targets::parse_target(&target, kind)?;
            let (machine_id, root_path) =
                resolve_target_identity(&conn, &parsed, machine_label.as_deref())?;
            match db::target_status(&conn, &machine_id, &root_path)? {
                Some(status) => print_target_status(&parsed, status, output.json),
                None => {
                    print_unknown_target_status(&parsed, &root_path, output.json)?;
                }
            }
        }
        Some(Commands::Tui) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            let open_root_provider = open_root_provider(db.clone(), machine_label.clone());
            tui::run_with_options(&conn, &db, machine_label, open_root_provider).await?;
        }
    }

    Ok(())
}

fn delete_database_file(db_path: &std::path::Path, yes: bool) -> anyhow::Result<()> {
    let sidecars = sqlite_sidecar_paths(db_path);
    if !yes {
        println!("db:\t{}", db_path.display());
        println!("exists:\t{}", db_path.exists());
        for sidecar in &sidecars {
            println!("sidecar:\t{}\t{}", sidecar.display(), sidecar.exists());
        }
        println!("confirm:\trerun with db delete --yes to remove this database file");
        return Ok(());
    }

    let mut removed = 0usize;
    for path in std::iter::once(db_path.to_path_buf()).chain(sidecars) {
        match fs::remove_file(&path) {
            Ok(()) => {
                removed += 1;
                println!("removed:\t{}", path.display());
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                println!("missing:\t{}", path.display());
            }
            Err(err) => {
                return Err(err).with_context(|| format!("removing {}", path.display()));
            }
        }
    }
    println!("deleted:\t{removed} file(s)");
    Ok(())
}

fn sqlite_sidecar_paths(db_path: &std::path::Path) -> Vec<PathBuf> {
    ["-wal", "-shm"]
        .into_iter()
        .map(|suffix| {
            let mut path = db_path.as_os_str().to_os_string();
            path.push(suffix);
            PathBuf::from(path)
        })
        .collect()
}

fn open_root_provider(db_path: PathBuf, machine_label: Option<String>) -> tui::OpenRootProvider {
    Arc::new(move |target| open_root_location(&db_path, machine_label.as_deref(), target))
}

fn open_root_location(
    db_path: &std::path::Path,
    machine_label: Option<&str>,
    target: &str,
) -> anyhow::Result<tui::OpenRootResult> {
    let conn = db::open_or_create(db_path)?;
    db::init_schema(&conn)?;
    let target_path = std::path::Path::new(target);
    if root_snapshot::looks_like_snapshot_file(target_path) {
        let result = root_snapshot::import_snapshot_file(&conn, target_path)?;
        return Ok(tui::OpenRootResult {
            initial_browse: None,
            selected_root_id: Some(result.root_id),
            status: format!(
                "imported root snapshot {} ({} files)",
                result.root_path, result.file_count
            ),
        });
    }
    let parsed = targets::parse_target(target, None)?;
    let (machine_id, root_path) = resolve_target_identity(&conn, &parsed, machine_label)?;
    if let Some(root) = db::find_root_by_machine_path(&conn, &machine_id, &root_path)? {
        return Ok(tui::OpenRootResult {
            initial_browse: None,
            selected_root_id: Some(root.id),
            status: format!("opened existing root {}", root_path),
        });
    }
    match parsed.kind {
        TargetKind::LocalPath | TargetKind::FileUrl => {
            let local_path = parsed
                .local_path()
                .ok_or_else(|| anyhow::anyhow!("target is not local file-like"))?;
            let summary = fswork::scan_to_db(
                &conn,
                &local_path,
                db_path,
                machine_label,
                fswork::OutputOptions {
                    quiet: true,
                    ..fswork::OutputOptions::default()
                },
            )?;
            let root_id = db::ensure_root(&conn, &machine_id, &root_path)?;
            Ok(tui::OpenRootResult {
                initial_browse: None,
                selected_root_id: Some(root_id),
                status: format!(
                    "opened {}: fast scanned {} files",
                    root_path, summary.files_seen
                ),
            })
        }
        TargetKind::Ssh => {
            let browse_provider = ssh_browse_provider(parsed.clone());
            let import_provider =
                ssh_import_provider(parsed.clone(), machine_id.clone(), db_path.to_path_buf());
            let entries = fast_scan_ssh_directory(&parsed, &root_path)?;
            Ok(tui::OpenRootResult {
                initial_browse: Some(tui::InitialBrowse {
                    label: parsed.original.clone(),
                    machine_id,
                    root_path: root_path.clone(),
                    current_path: root_path.clone(),
                    entries: entries.iter().map(tui_entry_from_fast).collect(),
                    browse_provider: Some(browse_provider),
                    import_provider: Some(import_provider),
                }),
                selected_root_id: None,
                status: format!("opened temporary SSH browse root {}", parsed.original),
            })
        }
        TargetKind::Url => {
            anyhow::bail!("URL browsing is not implemented yet");
        }
    }
}

fn resolve_registered_root(
    conn: &rusqlite::Connection,
    target: &str,
    kind: Option<TargetKind>,
    machine_label: Option<&str>,
) -> anyhow::Result<db::RootRow> {
    let parsed = targets::parse_target(target, kind)?;
    let (machine_id, root_path) = resolve_target_identity(conn, &parsed, machine_label)?;
    db::find_root_by_machine_path(conn, &machine_id, &root_path)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "target is not a known root yet: {target}. Run `gremlin target add {target}` or scan it first"
            )
        })
}

struct DefaultTargetResult {
    db_path: std::path::PathBuf,
    initial_browse: Option<tui::InitialBrowse>,
}

fn run_default_target(
    config_ctx: &config::ConfigContext,
    cli_db: Option<std::path::PathBuf>,
    target: &str,
    machine_label: Option<&str>,
    output: fswork::OutputOptions,
    will_open_tui: bool,
) -> anyhow::Result<DefaultTargetResult> {
    let text_output = fswork::OutputOptions {
        json: false,
        ..output
    };
    let db_path = config_ctx.resolve_db_or_default(cli_db)?;
    let conn = db::open_or_create(&db_path)?;
    db::init_schema(&conn)?;
    let parsed = targets::parse_target(target, None)?;
    let (machine_id, root_path) = resolve_target_identity(&conn, &parsed, machine_label)?;
    let mut initial_browse = None;
    match parsed.kind {
        TargetKind::LocalPath | TargetKind::FileUrl => {
            let root_id = db::ensure_root(&conn, &machine_id, &root_path)?;
            println!("db:\t{}", db_path.display());
            println!("target:\t{}", parsed.original);
            println!("root:\t{root_id}");
            let local_path = parsed
                .local_path()
                .ok_or_else(|| anyhow::anyhow!("target is not local file-like"))?;
            fswork::scan_to_db(&conn, &local_path, &db_path, machine_label, text_output)?;
            if let Some(status) = db::target_status(&conn, &machine_id, &root_path)? {
                print_target_status(&parsed, status, false);
            }
            println!(
                "next:\tgremlin hash {} --db {}",
                parsed.original,
                db_path.display()
            );
        }
        TargetKind::Ssh => {
            let host = parsed
                .machine_hint
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("SSH target missing machine hint"))?;
            if will_open_tui {
                ensure_passwordless_ssh_or_offer_copy_id(host)?;
            }
            println!("db:\t{}", db_path.display());
            println!(
                "target {:?}\tmachine={}\troot=temporary\tpath={}",
                parsed.kind, machine_id, root_path
            );
            if !will_open_tui {
                println!(
                    "next:\tuse `gremlin target add {}` to persist this root",
                    parsed.original
                );
                return Ok(DefaultTargetResult {
                    db_path,
                    initial_browse,
                });
            }
            let browse_provider = ssh_browse_provider(parsed.clone());
            let import_provider =
                ssh_import_provider(parsed.clone(), machine_id.clone(), db_path.clone());
            match fast_scan_ssh_directory(&parsed, &root_path) {
                Ok(entries) => {
                    if entries.is_empty() {
                        println!("empty:\t.");
                    }
                    for entry in &entries {
                        print_fast_directory_entry(entry);
                    }
                    initial_browse = Some(tui::InitialBrowse {
                        label: parsed.original.clone(),
                        machine_id: machine_id.clone(),
                        root_path: root_path.clone(),
                        current_path: root_path.clone(),
                        entries: entries.iter().map(tui_entry_from_fast).collect(),
                        browse_provider: Some(browse_provider),
                        import_provider: Some(import_provider),
                    });
                }
                Err(err) => {
                    println!("warning:\tSSH fastscan failed: {err}");
                    initial_browse = Some(tui::InitialBrowse {
                        label: parsed.original.clone(),
                        machine_id: machine_id.clone(),
                        root_path: root_path.clone(),
                        current_path: root_path.clone(),
                        entries: Vec::new(),
                        browse_provider: Some(browse_provider),
                        import_provider: Some(import_provider),
                    });
                }
            }
            println!(
                "next:\tuse `gremlin target add {}` to persist this root",
                parsed.original
            );
        }
        TargetKind::Url => {
            println!("db:\t{}", db_path.display());
            println!(
                "target {:?}\tmachine={}\troot=temporary\tpath={}",
                parsed.kind, machine_id, root_path
            );
            println!("next:\tURL browsing is not implemented yet");
        }
    }
    Ok(DefaultTargetResult {
        db_path,
        initial_browse,
    })
}

fn resolve_target_identity(
    conn: &rusqlite::Connection,
    parsed: &ParsedTarget,
    machine_label: Option<&str>,
) -> anyhow::Result<(String, String)> {
    match parsed.kind {
        TargetKind::LocalPath | TargetKind::FileUrl => {
            let local_path = parsed
                .local_path()
                .ok_or_else(|| anyhow::anyhow!("target is not local file-like"))?;
            let path = util::absolute_path(&local_path)?;
            let machine_id = db::ensure_local_machine_with_label(conn, machine_label)?;
            Ok((machine_id, util::lossy(&path)))
        }
        TargetKind::Ssh => {
            let label = parsed
                .machine_hint
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("SSH target missing machine hint"))?;
            let machine_id = db::ensure_machine_hint(conn, label, Some("ssh"))?;
            Ok((machine_id, parsed.path.clone()))
        }
        TargetKind::Url => {
            let label = parsed
                .scheme
                .as_deref()
                .map(|scheme| format!("{scheme} target"))
                .unwrap_or_else(|| "url target".to_string());
            let machine_id = db::ensure_machine_hint(conn, &label, parsed.scheme.as_deref())?;
            Ok((machine_id, parsed.path.clone()))
        }
    }
}

fn ensure_passwordless_ssh_or_offer_copy_id(host: &str) -> anyhow::Result<()> {
    match probe_passwordless_ssh(host) {
        Ok(()) => return Ok(()),
        Err(first_err) => {
            eprintln!("ssh:\tpasswordless login to {host} failed: {first_err}");
        }
    }

    eprintln!("ssh:\tGremlin needs passwordless SSH before opening the TUI for {host}.");
    eprintln!("ssh:\tRun `ssh-copy-id {host}` now to install your default public key?");
    if !confirm_stdin("Run ssh-copy-id? [y/N] ")? {
        anyhow::bail!("passwordless SSH is required before opening the TUI for {host}");
    }

    let status = Command::new("ssh-copy-id")
        .arg(host)
        .status()
        .with_context(|| format!("running ssh-copy-id {host}"))?;
    if !status.success() {
        anyhow::bail!("ssh-copy-id {host} exited with {status}");
    }

    probe_passwordless_ssh(host).with_context(|| {
        format!("passwordless SSH still failed after running ssh-copy-id for {host}")
    })
}

fn probe_passwordless_ssh(host: &str) -> anyhow::Result<()> {
    let output = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg(host)
        .arg("true")
        .output()
        .with_context(|| format!("probing passwordless SSH for {host}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if detail.is_empty() {
        anyhow::bail!("ssh exited with {}", output.status);
    }
    anyhow::bail!("{detail}");
}

fn confirm_stdin(prompt: &str) -> anyhow::Result<bool> {
    eprint!("{prompt}");
    io::stderr().flush().context("flushing prompt")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("reading confirmation")?;
    Ok(is_yes(input.trim()))
}

fn is_yes(input: &str) -> bool {
    matches!(input, "y" | "Y" | "yes" | "YES" | "Yes")
}

#[derive(Debug, Serialize)]
struct TargetStatusJson<'a> {
    target: &'a str,
    kind: TargetKind,
    known: bool,
    machine_id: &'a str,
    root_id: &'a str,
    path: &'a str,
    files: i64,
    bytes: i64,
    content_objects: i64,
    latest_event: Option<&'a str>,
    latest_job: Option<JobStatusJson<'a>>,
}

#[derive(Debug, Serialize)]
struct UnknownTargetStatusJson<'a> {
    target: &'a str,
    kind: TargetKind,
    known: bool,
    machine: String,
    path: &'a str,
    next: String,
}

#[derive(Debug, Serialize)]
struct JobStatusJson<'a> {
    id: &'a str,
    kind: &'a str,
    status: &'a str,
}

fn print_target_status(parsed: &ParsedTarget, status: db::TargetStatus, json: bool) {
    if json {
        let latest_event = status.latest_event_at.as_deref();
        let latest_job = status.latest_job.as_ref().map(|job| JobStatusJson {
            id: &job.id,
            kind: &job.kind,
            status: &job.status,
        });
        let payload = TargetStatusJson {
            target: &parsed.original,
            kind: parsed.kind,
            known: true,
            machine_id: &status.root.machine_id,
            root_id: &status.root.id,
            path: &status.root.path,
            files: status.file_count,
            bytes: status.total_bytes,
            content_objects: status.content_count,
            latest_event,
            latest_job,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).expect("serializing status should not fail")
        );
        return;
    }

    println!("target:\t{}", parsed.original);
    println!("kind:\t{:?}", parsed.kind);
    println!("known:\tyes");
    println!("machine_id:\t{}", status.root.machine_id);
    println!("root_id:\t{}", status.root.id);
    println!("path:\t{}", status.root.path);
    println!("files:\t{}", status.file_count);
    println!("bytes:\t{}", status.total_bytes);
    println!("content_objects:\t{}", status.content_count);
    println!(
        "latest_event:\t{}",
        status.latest_event_at.unwrap_or_else(|| "-".to_string())
    );
    if let Some(job) = status.latest_job {
        println!("latest_job:\t{}\t{}\t{}", job.id, job.kind, job.status);
    } else {
        println!("latest_job:\t-");
    }
}

fn print_unknown_target_status(
    parsed: &ParsedTarget,
    root_path: &str,
    json: bool,
) -> anyhow::Result<()> {
    let next = format!("gremlin target add {} --db <db>", parsed.original);
    if json {
        let payload = UnknownTargetStatusJson {
            target: &parsed.original,
            kind: parsed.kind,
            known: false,
            machine: parsed.display_machine_label(),
            path: root_path,
            next,
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("target:\t{}", parsed.original);
    println!("kind:\t{:?}", parsed.kind);
    println!("machine:\t{}", parsed.display_machine_label());
    println!("path:\t{}", root_path);
    println!("known:\tno");
    println!("next:\t{next}");
    Ok(())
}

fn print_transfer_plan(
    conn: &rusqlite::Connection,
    source: &db::RootRow,
    dest: &db::RootRow,
    result: transfer::TransferPlanResult,
    output: fswork::OutputOptions,
) -> anyhow::Result<()> {
    println!("transfer_plan:\t{}", result.plan_id);
    println!("job:\t{}", result.job_id);
    println!("source_root:\t{}\t{}", source.id, source.path);
    println!("dest_root:\t{}\t{}", dest.id, dest.path);
    println!("selection_set:\t{}", result.selection_set_id);
    println!(
        "marked:\t{}\t{}",
        result.marked_count,
        util::human_size(result.marked_bytes as u64)
    );
    for row in result.summary {
        println!(
            "{}:\t{}\t{}",
            row.action,
            row.files,
            util::human_size(row.bytes as u64)
        );
    }
    if output.details {
        for entry in db::transfer_plan_entries(conn, &result.plan_id)?
            .into_iter()
            .take(output.limit)
        {
            print_transfer_entry(&entry);
        }
    }
    println!("note:\tplanning only; no files were copied");
    Ok(())
}

fn print_collection_verify_summary(
    summary: collections::CollectionVerifySummary,
    output: fswork::OutputOptions,
) -> anyhow::Result<()> {
    if output.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("collection:\t{}", summary.collection_id);
    println!("name:\t{}", summary.collection_name);
    println!("root:\t{}\t{}", summary.root_id, summary.root_path);
    println!("entries:\t{}", summary.entries);
    println!("ok:\t{}", summary.ok);
    println!("size_only:\t{}", summary.size_only);
    println!("missing:\t{}", summary.missing);
    println!("size_mismatch:\t{}", summary.size_mismatch);
    println!("hash_mismatch:\t{}", summary.hash_mismatch);
    println!("unverified:\t{}", summary.unverified);
    println!("extras:\t{}", summary.extras);
    if output.details {
        for finding in summary.findings.into_iter().take(output.limit) {
            println!(
                "finding:\t{}\t{}\texpected={}\tactual={}",
                finding.kind.as_str(),
                finding.relative_path,
                util::human_size(finding.expected_size_bytes),
                finding
                    .actual_size_bytes
                    .map(util::human_size)
                    .unwrap_or_else(|| "-".to_string())
            );
        }
        for extra in summary.extra_files.into_iter().take(output.limit) {
            println!(
                "extra:\t{}\t{}",
                extra.relative_path,
                util::human_size(extra.size_bytes)
            );
        }
    }
    Ok(())
}

fn print_cached_directory_entry(entry: &db::CachedDirectoryEntry) {
    if entry.kind == "dir" {
        println!(
            "dir:\t{}\t{}\t{} files\t{}",
            entry.name,
            entry.relative_path,
            entry.file_count,
            util::human_size(entry.size_bytes as u64)
        );
    } else {
        println!(
            "file:\t{}\t{}\t{}\t{}\t{}\t{}",
            entry.name,
            entry.relative_path,
            util::human_size(entry.size_bytes as u64),
            entry.status.as_deref().unwrap_or("-"),
            entry.modified_at.as_deref().unwrap_or("-"),
            entry
                .content_id
                .as_deref()
                .map(short_id)
                .unwrap_or("stat-only")
        );
    }
}

fn print_root_delete_summary(root: &db::RootRow, summary: &db::RootDeleteSummary) {
    println!("root:\t{}\t{}\t{}", root.id, root.path, root.machine_id);
    print_root_delete_counts(summary);
    println!("note:\tthis removes Gremlin database records only; filesystem files are not deleted");
}

fn print_root_delete_counts(summary: &db::RootDeleteSummary) {
    println!(
        "records:\tobservations={} chunks={} selections={}/{} plans={}/{} checksums={}/{} jobs={} events={}",
        summary.path_observations,
        summary.chunk_hashes,
        summary.selection_sets,
        summary.selection_entries,
        summary.transfer_plans,
        summary.transfer_plan_entries,
        summary.checksum_collections,
        summary.checksum_entries,
        summary.jobs,
        summary.job_events
    );
}

#[derive(Debug)]
struct FastDirectoryEntry {
    kind: String,
    name: String,
    size_bytes: u64,
    modified_at: Option<String>,
}

fn ssh_browse_provider(parsed: ParsedTarget) -> tui::BrowseProvider {
    Arc::new(move |remote_path: &str| {
        fast_scan_ssh_directory(&parsed, remote_path)
            .map(|entries| entries.iter().map(tui_entry_from_fast).collect())
    })
}

fn ssh_import_provider(
    parsed: ParsedTarget,
    machine_id: String,
    db_path: PathBuf,
) -> tui::ImportProvider {
    Arc::new(move |mode, remote_path, progress| {
        import_ssh_root(&db_path, &parsed, &machine_id, remote_path, mode, progress)
    })
}

fn tui_entry_from_fast(entry: &FastDirectoryEntry) -> tui::InitialBrowseEntry {
    tui::InitialBrowseEntry {
        kind: entry.kind.clone(),
        name: entry.name.clone(),
        size_bytes: entry.size_bytes,
        modified_at: entry.modified_at.clone(),
    }
}

fn import_ssh_root(
    db_path: &std::path::Path,
    parsed: &ParsedTarget,
    machine_id: &str,
    remote_path: &str,
    mode: tui::ImportMode,
    progress: tui::ImportProgressCallback,
) -> anyhow::Result<tui::ImportResult> {
    let conn = db::open_existing(db_path)?;
    let host = parsed
        .machine_hint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("SSH target missing machine hint"))?;
    let execution_path = resolve_ssh_absolute_path(host, remote_path)?;
    let root_path = ssh_root_display_path(host, &execution_path);
    let root_id = db::ensure_root(&conn, machine_id, &root_path)?;
    emit_import_progress(
        &progress,
        &root_id,
        &root_path,
        "import starting",
        0,
        0,
        None,
    );
    let files_imported = match mode {
        tui::ImportMode::No => 0,
        tui::ImportMode::Fast => import_ssh_fast_stat(
            &conn,
            parsed,
            machine_id,
            &root_id,
            &root_path,
            &execution_path,
            &progress,
        )?,
        tui::ImportMode::Hash => import_ssh_native_hash(
            &conn,
            parsed,
            machine_id,
            &root_id,
            &execution_path,
            &root_path,
            &progress,
        )?,
    };
    Ok(tui::ImportResult {
        mode,
        root_id,
        root_path,
        files_imported,
    })
}

fn resolve_ssh_absolute_path(host: &str, remote_path: &str) -> anyhow::Result<String> {
    let shell_path = remote_shell_path(remote_path);
    let command = format!(
        "p={shell_path}; if [ -d \"$p\" ]; then cd \"$p\" && pwd -P; else d=$(dirname \"$p\") && b=$(basename \"$p\") && cd \"$d\" && printf '%s/%s\\n' \"$(pwd -P)\" \"$b\"; fi"
    );
    let output = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=2")
        .arg(host)
        .arg(command)
        .output()
        .with_context(|| format!("resolving SSH path {host}:{remote_path}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "ssh path resolution failed for {host}:{remote_path}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let resolved = String::from_utf8(output.stdout)
        .context("remote path resolution output was not UTF-8")?
        .trim()
        .to_string();
    if resolved.is_empty() {
        anyhow::bail!("ssh path resolution returned an empty path for {host}:{remote_path}");
    }
    Ok(resolved)
}

fn ssh_root_display_path(host: &str, remote_path: &str) -> String {
    format!("{host}:{remote_path}")
}

#[derive(Debug, Clone)]
struct RemoteStatEntry {
    relative_path: String,
    size_bytes: u64,
    modified_at: Option<String>,
}

fn import_ssh_fast_stat(
    conn: &rusqlite::Connection,
    parsed: &ParsedTarget,
    machine_id: &str,
    root_id: &str,
    root_path: &str,
    remote_path: &str,
    progress: &tui::ImportProgressCallback,
) -> anyhow::Result<u64> {
    let entries = fast_scan_ssh_tree(parsed, remote_path)?;
    import_remote_stat_entries(conn, machine_id, root_id, root_path, progress, &entries)?;
    emit_import_progress(
        progress,
        root_id,
        root_path,
        "fast stat indexed",
        entries.len() as u64,
        0,
        None,
    );
    Ok(entries.len() as u64)
}

fn import_remote_stat_entries(
    conn: &rusqlite::Connection,
    machine_id: &str,
    root_id: &str,
    root_path: &str,
    progress: &tui::ImportProgressCallback,
    entries: &[RemoteStatEntry],
) -> anyhow::Result<()> {
    let mut reporter = ImportProgressReporter::new(
        progress,
        root_id,
        root_path,
        "fast stat indexing",
        entries
            .iter()
            .map(|entry| remote_parent(&entry.relative_path)),
    );
    for entry in entries {
        db::insert_path_observation(
            conn,
            db::PathObservationInput {
                machine_id,
                root_id,
                relative_path: &entry.relative_path,
                basename: remote_basename(&entry.relative_path),
                parent_path: &remote_parent(&entry.relative_path),
                size_bytes: entry.size_bytes,
                modified_at: entry.modified_at.as_deref(),
                content_id: None,
            },
        )?;
        reporter.record_path(&entry.relative_path, &remote_parent(&entry.relative_path));
    }
    reporter.finish();
    Ok(())
}

#[derive(Debug)]
struct RemoteHashEntry {
    relative_path: String,
    basename: String,
    parent_path: String,
    size_bytes: u64,
    modified_at: Option<String>,
    sha256: String,
    crc32: Option<String>,
    chunks: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
struct RemoteChunkHashSummary {
    job_id: String,
    root_id: String,
    root_path: String,
    chunk_size_bytes: u64,
    files_seen: u64,
    chunks_hashed: u64,
    bytes_hashed: u64,
    errors: u64,
    hashed_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct RemoteRootContext<'a> {
    machine_id: &'a str,
    root_id: &'a str,
    root_path: &'a str,
}

fn import_ssh_native_hash(
    conn: &rusqlite::Connection,
    parsed: &ParsedTarget,
    machine_id: &str,
    root_id: &str,
    remote_path: &str,
    root_path: &str,
    progress: &tui::ImportProgressCallback,
) -> anyhow::Result<u64> {
    let host = parsed
        .machine_hint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("SSH target missing machine hint"))?;
    let previous = db::path_observations_for_root(conn, machine_id, root_id)?
        .into_iter()
        .map(|row| (row.relative_path.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let stat_entries = fast_scan_ssh_tree(parsed, remote_path)?;
    import_remote_stat_entries(
        conn,
        machine_id,
        root_id,
        root_path,
        progress,
        &stat_entries,
    )?;
    let hash_entries = prioritized_remote_hash_entries(&stat_entries, &previous);
    emit_import_progress(
        progress,
        root_id,
        root_path,
        "remote hash starting",
        0,
        hash_entries.len() as u64,
        None,
    );
    if hash_entries.is_empty() {
        emit_import_progress(
            progress,
            root_id,
            root_path,
            "remote hash indexed",
            0,
            0,
            None,
        );
        return Ok(0);
    }
    let collection_id = db::create_checksum_collection(
        conn,
        &format!("ssh native hash {root_path}"),
        "ssh_native_hash",
        None,
    )?;
    db::attach_checksum_collection_target(conn, &collection_id, machine_id, root_id)?;
    let mut reporter = ImportProgressReporter::new(
        progress,
        root_id,
        root_path,
        "remote hash indexing",
        hash_entries
            .iter()
            .map(|entry| remote_parent(&entry.relative_path)),
    );
    let helper_result = stream_helper_hash_ssh_entries(
        host,
        remote_path,
        &hash_entries,
        false,
        None,
        |message| {
            emit_import_progress(
                progress,
                root_id,
                root_path,
                message,
                0,
                hash_entries.len() as u64,
                None,
            );
            Ok(())
        },
        |entry| {
            persist_remote_hash_entry(
                conn,
                RemoteRootContext {
                    machine_id,
                    root_id,
                    root_path,
                },
                Some(&collection_id),
                entry,
                Some(&mut reporter),
                None,
            )
        },
    );
    match helper_result {
        Ok(()) => {}
        Err(remote_helper::ssh::SshHelperError::Unavailable(message)) => {
            emit_import_progress(
                progress,
                root_id,
                root_path,
                &format!("remote helper unavailable; falling back to SHA-256 only: {message}"),
                reporter.files_imported,
                reporter.total_files.saturating_sub(reporter.files_imported),
                reporter.current_path.clone(),
            );
            stream_native_hash_ssh_entries(host, remote_path, &hash_entries, |entry| {
                persist_remote_hash_entry(
                    conn,
                    RemoteRootContext {
                        machine_id,
                        root_id,
                        root_path,
                    },
                    Some(&collection_id),
                    entry,
                    Some(&mut reporter),
                    None,
                )
            })?;
        }
        Err(remote_helper::ssh::SshHelperError::Session(err)) => return Err(err),
    }
    reporter.finish();
    Ok(reporter.files_imported)
}

fn remote_chunk_hash_to_db(
    conn: &rusqlite::Connection,
    parsed: &ParsedTarget,
    _machine_label: Option<&str>,
    chunk_size_bytes: u64,
    output: fswork::OutputOptions,
) -> anyhow::Result<RemoteChunkHashSummary> {
    if chunk_size_bytes == 0 {
        anyhow::bail!("chunk size must be greater than zero");
    }
    db::init_schema(conn)?;
    let host = parsed
        .machine_hint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("SSH target missing machine hint"))?;
    let remote_path = parsed.path.as_str();
    let execution_path = resolve_ssh_absolute_path(host, remote_path)?;
    let machine_id = db::ensure_machine_hint(conn, host, Some("ssh"))?;
    let root_path = ssh_root_display_path(host, &execution_path);
    let root_id = db::ensure_root(conn, &machine_id, &root_path)?;
    let job_id = db::create_job(
        conn,
        "chunk_hash",
        Some(&machine_id),
        Some(&root_id),
        serde_json::json!({
            "path": root_path,
            "remote_path": execution_path,
            "chunk_size_bytes": chunk_size_bytes,
            "algorithm": "md5",
            "source": "ssh_helper",
        }),
    )?;
    db::start_job(conn, &job_id)?;
    persist_simple_job_event(
        conn,
        &job_id,
        1,
        EventKind::JobStarted,
        EventPayload::Job {
            kind: "chunk_hash".to_string(),
            path: Some(root_path.clone()),
            message: Some(format!(
                "remote helper md5 chunks of {} bytes",
                chunk_size_bytes
            )),
            files_seen: None,
            errors: None,
        },
    )?;

    let entries = fast_scan_ssh_tree(parsed, &execution_path)?;
    let progress: tui::ImportProgressCallback = Arc::new(|_| {});
    import_remote_stat_entries(conn, &machine_id, &root_id, &root_path, &progress, &entries)?;

    let files_seen = std::cell::Cell::new(0_u64);
    let chunks_hashed = std::cell::Cell::new(0_u64);
    let bytes_hashed = std::cell::Cell::new(0_u64);
    let errors = std::cell::Cell::new(0_u64);
    let hashed_paths = std::cell::RefCell::new(Vec::new());
    let helper_result = stream_helper_hash_ssh_entries(
        host,
        &execution_path,
        &entries,
        true,
        Some(chunk_size_bytes),
        |message| {
            errors.set(errors.get() + 1);
            eprintln!("remote chunk hash:\t{message}");
            Ok(())
        },
        |entry| {
            let chunk_count = entry
                .chunks
                .as_ref()
                .map_or(0_u64, |chunks| chunks.len() as u64);
            let size_bytes = entry.size_bytes;
            let relative_path = entry.relative_path.clone();
            persist_remote_hash_entry(
                conn,
                RemoteRootContext {
                    machine_id: &machine_id,
                    root_id: &root_id,
                    root_path: &root_path,
                },
                None,
                entry,
                None,
                Some((&job_id, chunk_size_bytes)),
            )?;
            files_seen.set(files_seen.get() + 1);
            chunks_hashed.set(chunks_hashed.get() + chunk_count);
            bytes_hashed.set(bytes_hashed.get() + size_bytes);
            hashed_paths.borrow_mut().push(relative_path);
            db::update_job_progress(
                conn,
                &job_id,
                db::JobProgressInput {
                    phase: "processing",
                    current_path: hashed_paths.borrow().last().map(String::as_str),
                    files_total: Some(entries.len() as u64),
                    files_seen: files_seen.get(),
                    files_done: files_seen.get(),
                    files_skipped: 0,
                    errors: errors.get(),
                },
            )?;
            Ok(())
        },
    );
    if let Err(err) = helper_result {
        db::complete_job(conn, &job_id, "failed")?;
        return Err(anyhow::anyhow!("remote chunk hash failed: {err}"));
    }
    let status = if errors.get() == 0 {
        "completed"
    } else {
        "completed_with_errors"
    };
    db::update_job_progress(
        conn,
        &job_id,
        db::JobProgressInput {
            phase: "finalizing",
            current_path: None,
            files_total: Some(entries.len() as u64),
            files_seen: files_seen.get(),
            files_done: files_seen.get(),
            files_skipped: 0,
            errors: errors.get(),
        },
    )?;
    persist_simple_job_event(
        conn,
        &job_id,
        2,
        EventKind::JobCompleted,
        EventPayload::Job {
            kind: "chunk_hash".to_string(),
            path: Some(root_path.clone()),
            message: Some(status.to_string()),
            files_seen: Some(files_seen.get()),
            errors: Some(errors.get()),
        },
    )?;
    db::complete_job(conn, &job_id, status)?;
    let summary = RemoteChunkHashSummary {
        job_id,
        root_id,
        root_path,
        chunk_size_bytes,
        files_seen: files_seen.get(),
        chunks_hashed: chunks_hashed.get(),
        bytes_hashed: bytes_hashed.get(),
        errors: errors.get(),
        hashed_paths: hashed_paths.into_inner(),
    };
    print_remote_chunk_hash_summary(&summary, output)?;
    Ok(summary)
}

fn persist_remote_hash_entry(
    conn: &rusqlite::Connection,
    root: RemoteRootContext<'_>,
    collection_id: Option<&str>,
    entry: RemoteHashEntry,
    reporter: Option<&mut ImportProgressReporter<'_>>,
    chunk_job: Option<(&str, u64)>,
) -> anyhow::Result<()> {
    if let Some(collection_id) = collection_id {
        db::insert_checksum_entry(
            conn,
            db::ChecksumEntryInput {
                collection_id,
                relative_path: &entry.relative_path,
                basename: &entry.basename,
                size_bytes: entry.size_bytes,
                modified_at: entry.modified_at.as_deref(),
                blake3: None,
                sha256: Some(&entry.sha256),
                crc32: entry.crc32.as_deref(),
                metadata_json: serde_json::json!({
                    "source": "ssh_native_hash",
                    "root_path": root.root_path,
                }),
            },
        )?;
    }
    let content_id = match entry.crc32.as_deref() {
        Some(crc32) => {
            db::ensure_content_object_sha256_crc(conn, entry.size_bytes, &entry.sha256, crc32)?
        }
        None => db::ensure_content_object_sha256(conn, entry.size_bytes, &entry.sha256)?,
    };
    db::insert_path_observation(
        conn,
        db::PathObservationInput {
            machine_id: root.machine_id,
            root_id: root.root_id,
            relative_path: &entry.relative_path,
            basename: &entry.basename,
            parent_path: &entry.parent_path,
            size_bytes: entry.size_bytes,
            modified_at: entry.modified_at.as_deref(),
            content_id: Some(&content_id),
        },
    )?;
    if let Some((job_id, chunk_size_bytes)) = chunk_job {
        persist_remote_chunk_hashes(conn, root.root_id, &entry, job_id, chunk_size_bytes)?;
    }
    if let Some(reporter) = reporter {
        reporter.record_path(&entry.relative_path, &entry.parent_path);
    }
    Ok(())
}

fn persist_remote_chunk_hashes(
    conn: &rusqlite::Connection,
    root_id: &str,
    entry: &RemoteHashEntry,
    job_id: &str,
    chunk_size_bytes: u64,
) -> anyhow::Result<()> {
    let Some(chunks) = entry.chunks.as_deref() else {
        anyhow::bail!(
            "remote helper returned no chunk hashes for {}",
            entry.relative_path
        );
    };
    let Some(path_observation_id) = db::path_observation_id(conn, root_id, &entry.relative_path)?
    else {
        anyhow::bail!(
            "path observation missing after remote chunk hash insert: {}",
            entry.relative_path
        );
    };
    let inputs = remote_chunk_hash_inputs(entry.size_bytes, chunk_size_bytes, chunks, job_id);
    db::replace_observation_chunk_hashes(
        conn,
        &path_observation_id,
        chunk_size_bytes,
        "md5",
        &inputs,
    )?;
    Ok(())
}

fn remote_chunk_hash_inputs<'a>(
    file_size: u64,
    chunk_size_bytes: u64,
    chunks: &'a [String],
    job_id: &'a str,
) -> Vec<db::ObservationChunkHashInput<'a>> {
    chunks
        .iter()
        .enumerate()
        .map(|(index, digest)| {
            let offset = index as u64 * chunk_size_bytes;
            db::ObservationChunkHashInput {
                chunk_size_bytes,
                chunk_index: index as u64,
                offset_bytes: offset,
                size_bytes: file_size.saturating_sub(offset).min(chunk_size_bytes),
                algorithm: "md5",
                digest,
                job_id: Some(job_id),
            }
        })
        .collect()
}

fn persist_simple_job_event(
    conn: &rusqlite::Connection,
    job_id: &str,
    sequence: i64,
    event_kind: EventKind,
    payload: EventPayload,
) -> anyhow::Result<()> {
    db::persist_event(
        conn,
        &EventEnvelope {
            event_kind,
            job_id: Some(job_id.to_string()),
            sequence: Some(sequence),
            created_at: util::now_rfc3339(),
            payload,
        },
    )?;
    Ok(())
}

fn print_remote_chunk_hash_summary(
    summary: &RemoteChunkHashSummary,
    options: fswork::OutputOptions,
) -> anyhow::Result<()> {
    if options.quiet {
        return Ok(());
    }
    if options.json {
        println!("{}", serde_json::to_string_pretty(summary)?);
        return Ok(());
    }
    println!(
        "chunk hash job {}: {} files, {} chunks, {}, {} errors, chunk_size={}",
        summary.job_id,
        summary.files_seen,
        summary.chunks_hashed,
        util::human_size(summary.bytes_hashed),
        summary.errors,
        util::human_size(summary.chunk_size_bytes)
    );
    let limit = if options.details {
        summary.hashed_paths.len()
    } else {
        options.limit
    };
    for path in summary.hashed_paths.iter().take(limit) {
        println!("chunked\t{path}");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RemoteHashPriority {
    StaleMetadata,
    MissingHash,
}

fn prioritized_remote_hash_entries(
    entries: &[RemoteStatEntry],
    previous: &BTreeMap<String, db::PathObservationRow>,
) -> Vec<RemoteStatEntry> {
    let mut needed = entries
        .iter()
        .filter_map(|entry| {
            remote_hash_priority(entry, previous.get(&entry.relative_path))
                .map(|priority| (priority, entry.clone()))
        })
        .collect::<Vec<_>>();
    needed.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.relative_path.cmp(&right.1.relative_path))
    });
    needed.into_iter().map(|(_, entry)| entry).collect()
}

fn remote_hash_priority(
    entry: &RemoteStatEntry,
    previous: Option<&db::PathObservationRow>,
) -> Option<RemoteHashPriority> {
    match previous {
        None => Some(RemoteHashPriority::MissingHash),
        Some(previous)
            if previous.size_bytes != entry.size_bytes
                || previous.modified_at != entry.modified_at =>
        {
            Some(RemoteHashPriority::StaleMetadata)
        }
        Some(previous) if previous.content_id.is_none() => Some(RemoteHashPriority::MissingHash),
        Some(_) => None,
    }
}

struct ImportProgressReporter<'a> {
    callback: &'a tui::ImportProgressCallback,
    root_id: &'a str,
    root_path: &'a str,
    phase: &'static str,
    last_emit: Instant,
    dirty: bool,
    files_imported: u64,
    total_files: u64,
    processed_dirs: BTreeSet<String>,
    queued_dir_file_counts: BTreeMap<String, u64>,
    current_path: Option<String>,
}

impl<'a> ImportProgressReporter<'a> {
    fn new<I>(
        callback: &'a tui::ImportProgressCallback,
        root_id: &'a str,
        root_path: &'a str,
        phase: &'static str,
        queued_parents: I,
    ) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let mut queued_dir_file_counts = BTreeMap::new();
        let mut total_files = 0;
        for parent in queued_parents {
            *queued_dir_file_counts.entry(parent).or_insert(0) += 1;
            total_files += 1;
        }
        Self {
            callback,
            root_id,
            root_path,
            phase,
            last_emit: Instant::now() - Duration::from_millis(250),
            dirty: false,
            files_imported: 0,
            total_files,
            processed_dirs: BTreeSet::new(),
            queued_dir_file_counts,
            current_path: None,
        }
    }

    fn record_path(&mut self, current_path: &str, parent_path: &str) {
        self.files_imported += 1;
        self.current_path = Some(current_path.to_string());
        self.processed_dirs.insert(parent_path.to_string());
        if let Some(count) = self.queued_dir_file_counts.get_mut(parent_path) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.queued_dir_file_counts.remove(parent_path);
            }
        }
        self.dirty = true;
        if self.last_emit.elapsed() >= Duration::from_millis(250) {
            self.emit();
        }
    }

    fn finish(&mut self) {
        if self.dirty {
            self.emit();
        }
    }

    fn emit(&mut self) {
        (self.callback)(tui::ImportProgress {
            root_id: self.root_id.to_string(),
            root_path: self.root_path.to_string(),
            files_imported: self.files_imported,
            files_queued: self.total_files.saturating_sub(self.files_imported),
            directories_processed: self.processed_dirs.len() as u64,
            directories_queued: self.queued_dir_file_counts.len() as u64,
            current_path: self.current_path.clone(),
            phase: self.phase.to_string(),
        });
        self.last_emit = Instant::now();
        self.dirty = false;
    }
}

fn emit_import_progress(
    callback: &tui::ImportProgressCallback,
    root_id: &str,
    root_path: &str,
    phase: &str,
    files_imported: u64,
    files_queued: u64,
    current_path: Option<String>,
) {
    callback(tui::ImportProgress {
        root_id: root_id.to_string(),
        root_path: root_path.to_string(),
        files_imported,
        files_queued,
        directories_processed: 0,
        directories_queued: 0,
        current_path,
        phase: phase.to_string(),
    });
}

fn stream_helper_hash_ssh_entries(
    host: &str,
    remote_path: &str,
    entries: &[RemoteStatEntry],
    include_chunks: bool,
    chunk_size_bytes: Option<u64>,
    mut on_error: impl FnMut(&str) -> anyhow::Result<()>,
    mut on_entry: impl FnMut(RemoteHashEntry) -> anyhow::Result<()>,
) -> Result<(), remote_helper::ssh::SshHelperError> {
    let root_is_directory = ssh_path_is_directory(host, remote_path).map_err(|err| {
        remote_helper::ssh::SshHelperError::Unavailable(format!(
            "could not classify remote root path {host}:{remote_path}: {err}"
        ))
    })?;
    let stat_by_id = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (index as u64, entry))
        .collect::<BTreeMap<_, _>>();
    let requests = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut hashes = vec!["crc32".to_string(), "sha256".to_string()];
            if include_chunks {
                hashes.push("chunks".to_string());
            }
            remote_helper::protocol::HashRequest {
                id: serde_json::json!(index as u64),
                op: "hash".to_string(),
                path: remote_hash_request_path(remote_path, entry, root_is_directory),
                hashes,
                chunk_size: chunk_size_bytes,
            }
        })
        .collect::<Vec<_>>();
    remote_helper::ssh::stream_hash_requests(host, &requests, |event| {
        match event {
            remote_helper::protocol::HelperEvent::Result {
                id,
                path,
                stable,
                sha256,
                crc32,
                chunks,
                ..
            } => {
                let Some(index) = id.as_u64() else {
                    on_error(&format!("remote helper returned non-numeric id for {path}"))?;
                    return Ok(());
                };
                let Some(stat) = stat_by_id.get(&index) else {
                    on_error(&format!(
                        "remote helper returned unknown id {index} for {path}"
                    ))?;
                    return Ok(());
                };
                if !stable {
                    on_error(&format!(
                        "remote helper skipped unstable file {}",
                        stat.relative_path
                    ))?;
                    return Ok(());
                }
                let Some(sha256) = sha256 else {
                    on_error(&format!(
                        "remote helper returned no SHA-256 for {}",
                        stat.relative_path
                    ))?;
                    return Ok(());
                };
                if include_chunks && chunks.is_none() {
                    on_error(&format!(
                        "remote helper returned no chunk hashes for {}",
                        stat.relative_path
                    ))?;
                    return Ok(());
                }
                on_entry(RemoteHashEntry {
                    relative_path: stat.relative_path.clone(),
                    basename: remote_basename(&stat.relative_path).to_string(),
                    parent_path: remote_parent(&stat.relative_path),
                    size_bytes: stat.size_bytes,
                    modified_at: stat.modified_at.clone(),
                    sha256,
                    crc32,
                    chunks,
                })?;
            }
            remote_helper::protocol::HelperEvent::Error {
                id,
                path,
                code,
                message,
            } => {
                let relative = id
                    .as_u64()
                    .and_then(|index| stat_by_id.get(&index))
                    .map(|entry| entry.relative_path.as_str())
                    .or(path.as_deref())
                    .unwrap_or("-");
                on_error(&format!(
                    "remote helper skipped {relative}: {code}: {message}"
                ))?;
            }
            remote_helper::protocol::HelperEvent::Progress { .. }
            | remote_helper::protocol::HelperEvent::Hello { .. } => {}
        }
        Ok(())
    })
}

fn remote_hash_request_path(
    remote_path: &str,
    entry: &RemoteStatEntry,
    root_is_directory: bool,
) -> String {
    if !root_is_directory {
        return remote_path.to_string();
    }
    if remote_path.ends_with('/') {
        format!("{remote_path}{}", entry.relative_path)
    } else {
        format!("{remote_path}/{}", entry.relative_path)
    }
}

fn ssh_path_is_directory(host: &str, remote_path: &str) -> anyhow::Result<bool> {
    let status = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=2")
        .arg(host)
        .arg(format!("test -d {}", remote_shell_path(remote_path)))
        .status()
        .with_context(|| format!("classifying SSH path {host}:{remote_path}"))?;
    Ok(status.success())
}

fn stream_native_hash_ssh_entries(
    host: &str,
    remote_path: &str,
    entries: &[RemoteStatEntry],
    mut on_entry: impl FnMut(RemoteHashEntry) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    for batch in remote_hash_batches(entries) {
        let stat_by_path = batch
            .iter()
            .map(|entry| (entry.relative_path.as_str(), *entry))
            .collect::<BTreeMap<_, _>>();
        let command = remote_hash_batch_command(remote_path, &batch);
        stream_ssh_lines(
            host,
            &command,
            format!("hashing selected SSH target entries {host}:{remote_path}"),
            |line| {
                if let Some(entry) = parse_native_hash_batch_line(remote_path, &stat_by_path, line)
                {
                    on_entry(entry)?;
                }
                Ok(())
            },
        )?;
    }
    Ok(())
}

fn remote_hash_batches(entries: &[RemoteStatEntry]) -> Vec<Vec<&RemoteStatEntry>> {
    const MAX_BATCH_COMMAND_CHARS: usize = 24_000;
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut current_len = 0_usize;
    for entry in entries {
        let quoted_len = shell_quote(&entry.relative_path).len() + 1;
        if !current.is_empty() && current_len + quoted_len > MAX_BATCH_COMMAND_CHARS {
            batches.push(current);
            current = Vec::new();
            current_len = 0;
        }
        current.push(entry);
        current_len += quoted_len;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn remote_hash_batch_command(remote_path: &str, entries: &[&RemoteStatEntry]) -> String {
    let quoted_entries = entries
        .iter()
        .map(|entry| shell_quote(&entry.relative_path))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "p={}; if [ -d \"$p\" ]; then cd \"$p\" || exit 1; for f in {}; do sum=$(sha256sum -- \"$f\") || exit $?; printf '%s\\t%s\\n' \"$f\" \"$sum\"; done; else sum=$(sha256sum -- \"$p\") || exit $?; printf '\\t%s\\n' \"$sum\"; fi",
        remote_shell_path(remote_path),
        quoted_entries
    )
}

fn stream_ssh_lines(
    host: &str,
    command: &str,
    context: String,
    mut on_line: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut child = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=2")
        .arg(host)
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| context.clone())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture ssh stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture ssh stderr"))?;
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    for line in BufReader::new(stdout).lines() {
        let line = line.with_context(|| context.clone())?;
        if !line.trim().is_empty() {
            on_line(&line)?;
        }
    }
    let status = child.wait().with_context(|| context.clone())?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        anyhow::bail!("ssh command failed for {host}: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(test)]
fn parse_native_hash_output(remote_path: &str, stdout: &str) -> Vec<RemoteHashEntry> {
    let mut entries = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        if let Some(entry) = parse_native_hash_line(remote_path, line) {
            entries.push(entry);
        }
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    entries
}

#[cfg(test)]
fn parse_native_hash_line(remote_path: &str, line: &str) -> Option<RemoteHashEntry> {
    let parts = line.splitn(4, '\t').collect::<Vec<_>>();
    if parts.len() != 4 {
        return None;
    }
    let sha256 = parts[3].split_whitespace().next()?;
    let relative_path = if parts[0].is_empty() {
        remote_path_basename(remote_path)
    } else {
        parts[0].to_string()
    };
    Some(RemoteHashEntry {
        basename: remote_basename(&relative_path).to_string(),
        parent_path: remote_parent(&relative_path),
        relative_path,
        size_bytes: parts[1].parse::<u64>().unwrap_or(0),
        modified_at: Some(parts[2].to_string()),
        sha256: sha256.to_string(),
        crc32: None,
        chunks: None,
    })
}

fn parse_native_hash_batch_line(
    remote_path: &str,
    stat_by_path: &BTreeMap<&str, &RemoteStatEntry>,
    line: &str,
) -> Option<RemoteHashEntry> {
    let (relative, sum) = line.split_once('\t')?;
    let sha256 = sum.split_whitespace().next()?;
    let relative_path = if relative.is_empty() {
        remote_path_basename(remote_path)
    } else {
        relative.to_string()
    };
    let stat = stat_by_path.get(relative_path.as_str())?;
    Some(RemoteHashEntry {
        basename: remote_basename(&relative_path).to_string(),
        parent_path: remote_parent(&relative_path),
        relative_path,
        size_bytes: stat.size_bytes,
        modified_at: stat.modified_at.clone(),
        sha256: sha256.to_string(),
        crc32: None,
        chunks: None,
    })
}

fn fast_scan_ssh_tree(
    parsed: &ParsedTarget,
    remote_path: &str,
) -> anyhow::Result<Vec<RemoteStatEntry>> {
    let host = parsed
        .machine_hint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("SSH target missing machine hint"))?;
    let command = format!(
        "find -L {} -type f -printf '%P\\t%s\\t%T+\\n'",
        remote_shell_path(remote_path)
    );
    let output = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=2")
        .arg(host)
        .arg(command)
        .output()
        .with_context(|| format!("fast importing SSH target {host}:{remote_path}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "ssh fast import failed for {host}:{remote_path}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let parts = line.splitn(3, '\t').collect::<Vec<_>>();
        if parts.len() != 3 {
            continue;
        }
        let relative_path = if parts[0].is_empty() {
            remote_path_basename(remote_path)
        } else {
            parts[0].to_string()
        };
        entries.push(RemoteStatEntry {
            relative_path,
            size_bytes: parts[1].parse::<u64>().unwrap_or(0),
            modified_at: Some(parts[2].to_string()),
        });
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(entries)
}

fn remote_basename(relative_path: &str) -> &str {
    relative_path.rsplit('/').next().unwrap_or(relative_path)
}

fn remote_path_basename(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && *name != "~")
        .unwrap_or(path)
        .to_string()
}

fn remote_parent(relative_path: &str) -> String {
    relative_path
        .rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                ".".to_string()
            } else {
                parent.to_string()
            }
        })
        .unwrap_or_else(|| ".".to_string())
}

fn fast_scan_ssh_directory(
    parsed: &ParsedTarget,
    remote_path: &str,
) -> anyhow::Result<Vec<FastDirectoryEntry>> {
    let host = parsed
        .machine_hint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("SSH target missing machine hint"))?;
    let command = format!(
        "find -L {} -mindepth 1 -maxdepth 1 -printf '%y\\t%s\\t%T+\\t%P\\n'",
        remote_shell_path(remote_path)
    );
    let output = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=2")
        .arg(host)
        .arg(command)
        .output()
        .with_context(|| format!("listing SSH target {host}:{remote_path}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "ssh listing failed for {host}:{remote_path}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let parts = line.splitn(4, '\t').collect::<Vec<_>>();
        if parts.len() != 4 {
            continue;
        }
        let kind = if parts[0] == "d" { "dir" } else { "file" };
        entries.push(FastDirectoryEntry {
            kind: kind.to_string(),
            name: parts[3].to_string(),
            size_bytes: parts[1].parse::<u64>().unwrap_or(0),
            modified_at: Some(parts[2].to_string()),
        });
    }
    entries.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

fn print_fast_directory_entry(entry: &FastDirectoryEntry) {
    if entry.kind == "dir" {
        println!(
            "dir:\t{}\t{}\t{}",
            entry.name,
            entry.modified_at.as_deref().unwrap_or("-"),
            util::human_size(entry.size_bytes)
        );
    } else {
        println!(
            "file:\t{}\t{}\t{}",
            entry.name,
            util::human_size(entry.size_bytes),
            entry.modified_at.as_deref().unwrap_or("-")
        );
    }
}

fn remote_child_path(root_path: &str, child_path: &str) -> String {
    let child = child_path.trim().trim_matches('/');
    if child.is_empty() || child == "." {
        return root_path.to_string();
    }
    if root_path == "~" {
        format!("~/{child}")
    } else {
        format!("{}/{}", root_path.trim_end_matches('/'), child)
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remote_shell_path(path: &str) -> String {
    if path == "~" {
        "$HOME".to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("$HOME/{}", shell_quote(rest))
    } else {
        shell_quote(path)
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(18)).unwrap_or(value)
}

fn print_transfer_plan_row(
    conn: &rusqlite::Connection,
    plan: &db::TransferPlanRow,
) -> anyhow::Result<()> {
    println!(
        "plan:\t{}\t{}\t{}\t{} files\t{}\t{} -> {}",
        plan.id,
        plan.status,
        plan.created_at,
        plan.entry_count,
        util::human_size(plan.total_bytes as u64),
        plan.source_path,
        plan.dest_path
    );
    println!(
        "roots:\tsource={}\tdest={}\tselection={}\tparams={}",
        plan.source_root_id,
        plan.dest_root_id,
        plan.selection_set_id.as_deref().unwrap_or("-"),
        plan.params_json.as_deref().unwrap_or("{}")
    );
    println!("job:\t{}", plan.job_id.as_deref().unwrap_or("-"));
    for row in db::transfer_plan_action_summary(conn, &plan.id)? {
        println!(
            "{}:\t{}\t{}",
            row.action,
            row.files,
            util::human_size(row.bytes as u64)
        );
    }
    Ok(())
}

fn print_transfer_entry(entry: &db::TransferPlanEntryRow) {
    println!(
        "entry:\t{}\t{}\t{}\t{}\tdest_path={}\tsource={}\tdest={}\tmetadata={}",
        entry.action,
        util::human_size(entry.size_bytes),
        entry.reason,
        entry.relative_path,
        entry.dest_relative_path,
        entry.source_content_id.as_deref().unwrap_or("-"),
        entry.dest_content_id.as_deref().unwrap_or("-"),
        entry.metadata_json
    );
}

fn export_root_sfv(
    conn: &rusqlite::Connection,
    root: &db::RootRow,
    output: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let entries = db::sfv_entries_for_root(conn, &root.id)?;
    if entries.is_empty() {
        anyhow::bail!(
            "root {} has no stored CRC32 metadata to export as SFV",
            root.path
        );
    }
    let file_count = entries.len();
    let mut text = String::new();
    text.push_str(&format!(
        "; Generated by gremlin from metadata for {}\n",
        root.path
    ));
    for entry in entries {
        text.push_str(&format!("{} {}\n", entry.relative_path, entry.crc32));
    }
    if let Some(output) = output {
        std::fs::write(output, text)
            .with_context(|| format!("writing SFV {}", output.display()))?;
        println!("exported_sfv:\t{}\t{}", root.id, output.display());
        println!("files:\t{file_count}");
    } else {
        print!("{text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_root_display_path_includes_host_and_path() {
        assert_eq!(
            ssh_root_display_path("nas01", "/srv/archive/photos"),
            "nas01:/srv/archive/photos"
        );
    }

    #[test]
    fn remote_path_basename_handles_host_qualified_file_paths() {
        assert_eq!(
            remote_path_basename("nas01:/srv/archive/foo.png"),
            "foo.png"
        );
        assert_eq!(remote_path_basename("/srv/archive/foo.png"), "foo.png");
    }

    #[test]
    fn confirmation_accepts_only_explicit_yes() {
        assert!(is_yes("y"));
        assert!(is_yes("yes"));
        assert!(is_yes("YES"));
        assert!(!is_yes(""));
        assert!(!is_yes("n"));
        assert!(!is_yes("sure"));
    }

    #[test]
    fn remote_chunk_hash_inputs_preserve_offsets_and_final_partial_size() {
        let digests = vec!["a".to_string(), "b".to_string(), "c".to_string()];

        let chunks = remote_chunk_hash_inputs(10, 4, &digests, "job_1");

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].offset_bytes, 0);
        assert_eq!(chunks[0].size_bytes, 4);
        assert_eq!(chunks[0].digest, "a");
        assert_eq!(chunks[1].chunk_index, 1);
        assert_eq!(chunks[1].offset_bytes, 4);
        assert_eq!(chunks[1].size_bytes, 4);
        assert_eq!(chunks[2].chunk_index, 2);
        assert_eq!(chunks[2].offset_bytes, 8);
        assert_eq!(chunks[2].size_bytes, 2);
        assert_eq!(chunks[2].algorithm, "md5");
        assert_eq!(chunks[2].job_id, Some("job_1"));
    }

    #[test]
    fn open_root_location_selects_existing_local_root() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("gremlin.db");
        let root_dir = dir.path().join("root");
        std::fs::create_dir(&root_dir).unwrap();
        let conn = db::open_or_create(&db_path).unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let root_path = util::lossy(&util::absolute_path(&root_dir).unwrap());
        let root_id = db::ensure_root(&conn, &machine_id, &root_path).unwrap();
        drop(conn);

        let result = open_root_location(&db_path, None, &root_dir.to_string_lossy()).unwrap();

        assert_eq!(result.selected_root_id.as_deref(), Some(root_id.as_str()));
        assert!(result.initial_browse.is_none());
        assert!(result.status.contains("opened existing root"));
    }

    #[test]
    fn open_root_location_imports_root_snapshot_json() {
        let dir = tempfile::tempdir().unwrap();
        let source_db = dir.path().join("source.db");
        let import_db = dir.path().join("import.db");
        let root_dir = dir.path().join("root");
        std::fs::create_dir(&root_dir).unwrap();
        let conn = db::open_or_create(&source_db).unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let root_path = util::lossy(&util::absolute_path(&root_dir).unwrap());
        let root_id = db::ensure_root(&conn, &machine_id, &root_path).unwrap();
        db::insert_path_observation(
            &conn,
            db::PathObservationInput {
                machine_id: &machine_id,
                root_id: &root_id,
                relative_path: "hello.txt",
                basename: "hello.txt",
                parent_path: ".",
                size_bytes: 5,
                modified_at: None,
                content_id: None,
            },
        )
        .unwrap();
        let root = db::root_by_id(&conn, &root_id).unwrap().unwrap();
        let snapshot_path = dir.path().join("root.json");
        root_snapshot::export_root_to_path(&conn, &root, &snapshot_path).unwrap();
        drop(conn);

        let result =
            open_root_location(&import_db, None, &snapshot_path.to_string_lossy()).unwrap();
        let imported = db::open_existing(&import_db).unwrap();

        assert!(result.initial_browse.is_none());
        assert!(result.status.contains("imported root snapshot"));
        assert_eq!(
            db::root_by_id(&imported, result.selected_root_id.as_deref().unwrap())
                .unwrap()
                .unwrap()
                .path,
            root_path
        );
    }

    #[test]
    fn parses_native_ssh_hash_output_for_directory_files() {
        let entries = parse_native_hash_output(
            "/srv/archive",
            "dir/a.txt\t5\t2026-07-08 12:00:00.000000000 +0000\tabcd1234  /srv/archive/dir/a.txt\n",
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative_path, "dir/a.txt");
        assert_eq!(entries[0].basename, "a.txt");
        assert_eq!(entries[0].parent_path, "dir");
        assert_eq!(entries[0].size_bytes, 5);
        assert_eq!(entries[0].sha256, "abcd1234");
    }

    #[test]
    fn parses_native_ssh_hash_output_for_root_file() {
        let entries = parse_native_hash_output(
            "/srv/archive/foo.png",
            "\t7\t2026-07-08 12:00:00.000000000 +0000\tffff  /srv/archive/foo.png\n",
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative_path, "foo.png");
        assert_eq!(entries[0].basename, "foo.png");
        assert_eq!(entries[0].parent_path, ".");
        assert_eq!(entries[0].size_bytes, 7);
        assert_eq!(entries[0].sha256, "ffff");
    }

    #[test]
    fn remote_hash_priority_skips_unchanged_and_prioritizes_stale_metadata() {
        let entries = vec![
            RemoteStatEntry {
                relative_path: "missing.txt".to_string(),
                size_bytes: 5,
                modified_at: Some("2026-07-08T00:00:00Z".to_string()),
            },
            RemoteStatEntry {
                relative_path: "stale-time.txt".to_string(),
                size_bytes: 5,
                modified_at: Some("2026-07-09T00:00:00Z".to_string()),
            },
            RemoteStatEntry {
                relative_path: "stale-size.txt".to_string(),
                size_bytes: 9,
                modified_at: Some("2026-07-08T00:00:00Z".to_string()),
            },
            RemoteStatEntry {
                relative_path: "unchanged.txt".to_string(),
                size_bytes: 5,
                modified_at: Some("2026-07-08T00:00:00Z".to_string()),
            },
        ];
        let previous = BTreeMap::from([
            (
                "missing.txt".to_string(),
                db::PathObservationRow {
                    relative_path: "missing.txt".to_string(),
                    size_bytes: 5,
                    modified_at: Some("2026-07-08T00:00:00Z".to_string()),
                    content_id: None,
                },
            ),
            (
                "stale-time.txt".to_string(),
                db::PathObservationRow {
                    relative_path: "stale-time.txt".to_string(),
                    size_bytes: 5,
                    modified_at: Some("2026-07-08T00:00:00Z".to_string()),
                    content_id: Some("content_old".to_string()),
                },
            ),
            (
                "stale-size.txt".to_string(),
                db::PathObservationRow {
                    relative_path: "stale-size.txt".to_string(),
                    size_bytes: 5,
                    modified_at: Some("2026-07-08T00:00:00Z".to_string()),
                    content_id: Some("content_old".to_string()),
                },
            ),
            (
                "unchanged.txt".to_string(),
                db::PathObservationRow {
                    relative_path: "unchanged.txt".to_string(),
                    size_bytes: 5,
                    modified_at: Some("2026-07-08T00:00:00Z".to_string()),
                    content_id: Some("content_ok".to_string()),
                },
            ),
        ]);

        let prioritized = prioritized_remote_hash_entries(&entries, &previous)
            .into_iter()
            .map(|entry| entry.relative_path)
            .collect::<Vec<_>>();

        assert_eq!(
            prioritized,
            vec!["stale-size.txt", "stale-time.txt", "missing.txt"]
        );
    }

    #[test]
    fn parses_selected_remote_hash_batch_output_with_stat_metadata() {
        let stat = RemoteStatEntry {
            relative_path: "dir/a file.txt".to_string(),
            size_bytes: 42,
            modified_at: Some("2026-07-08T00:00:00Z".to_string()),
        };
        let stat_by_path = BTreeMap::from([(stat.relative_path.as_str(), &stat)]);

        let entry = parse_native_hash_batch_line(
            "/srv/archive",
            &stat_by_path,
            "dir/a file.txt\tabcd1234  dir/a file.txt",
        )
        .unwrap();

        assert_eq!(entry.relative_path, "dir/a file.txt");
        assert_eq!(entry.basename, "a file.txt");
        assert_eq!(entry.parent_path, "dir");
        assert_eq!(entry.size_bytes, 42);
        assert_eq!(entry.modified_at.as_deref(), Some("2026-07-08T00:00:00Z"));
        assert_eq!(entry.sha256, "abcd1234");
    }
}
