mod cli;
mod collections;
mod config;
mod db;
mod error;
mod events;
mod fswork;
mod import;
mod targets;
mod transfer;
mod tui;
mod util;

use anyhow::Context;
use clap::Parser;
use cli::{
    Cli, Commands, ConfigCommands, JobCommands, TargetCommands, TransferCommands, WorkerCommands,
};
use serde::Serialize;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
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
                tui::run_with_options(&conn, &db, machine_label).await?;
                return Ok(());
            };
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
                tui::run_with_initial_browse(
                    &conn,
                    &default_target.db_path,
                    machine_label,
                    default_target.initial_browse,
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
            if !matches!(parsed.kind, TargetKind::LocalPath | TargetKind::FileUrl) {
                anyhow::bail!("chunk-hash currently supports local path and file:// targets only");
            }
            let local_path = parsed
                .local_path()
                .ok_or_else(|| anyhow::anyhow!("chunk-hash target is not local file-like"))?;
            let chunk_size_bytes = chunk_size_mib
                .checked_mul(1024 * 1024)
                .ok_or_else(|| anyhow::anyhow!("chunk size is too large"))?;
            fswork::chunk_hash_to_db(
                &conn,
                &local_path,
                &db,
                machine_label.as_deref(),
                chunk_size_bytes,
                output,
            )?;
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
                source_kind,
                dest_kind,
            } => {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let source_root =
                    resolve_registered_root(&conn, &source, source_kind, machine_label.as_deref())?;
                let dest_root =
                    resolve_registered_root(&conn, &dest, dest_kind, machine_label.as_deref())?;
                let result = transfer::plan_selected_files(&conn, &source_root, &dest_root)?;
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
            tui::run_with_options(&conn, &db, machine_label).await?;
        }
    }

    Ok(())
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
    Arc::new(move |mode, remote_path| {
        import_ssh_root(&db_path, &parsed, &machine_id, remote_path, mode)
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
) -> anyhow::Result<tui::ImportResult> {
    let conn = db::open_existing(db_path)?;
    let host = parsed
        .machine_hint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("SSH target missing machine hint"))?;
    let execution_path = resolve_ssh_absolute_path(host, remote_path)?;
    let root_path = ssh_root_display_path(host, &execution_path);
    let root_id = db::ensure_root(&conn, machine_id, &root_path)?;
    let files_imported = match mode {
        tui::ImportMode::No => 0,
        tui::ImportMode::Fast => {
            import_ssh_fast_stat(&conn, parsed, machine_id, &root_id, &execution_path)?
        }
        tui::ImportMode::Hash => import_ssh_native_hash(
            &conn,
            parsed,
            machine_id,
            &root_id,
            &execution_path,
            &root_path,
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

#[derive(Debug)]
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
    remote_path: &str,
) -> anyhow::Result<u64> {
    let entries = fast_scan_ssh_tree(parsed, remote_path)?;
    for entry in &entries {
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
    }
    Ok(entries.len() as u64)
}

#[derive(Debug)]
struct RemoteHashEntry {
    relative_path: String,
    basename: String,
    parent_path: String,
    size_bytes: u64,
    modified_at: Option<String>,
    sha256: String,
}

fn import_ssh_native_hash(
    conn: &rusqlite::Connection,
    parsed: &ParsedTarget,
    machine_id: &str,
    root_id: &str,
    remote_path: &str,
    root_path: &str,
) -> anyhow::Result<u64> {
    let host = parsed
        .machine_hint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("SSH target missing machine hint"))?;
    let entries = native_hash_ssh_tree(host, remote_path)?;
    let collection_id = db::create_checksum_collection(
        conn,
        &format!("ssh native hash {root_path}"),
        "ssh_native_hash",
        None,
    )?;
    db::attach_checksum_collection_target(conn, &collection_id, machine_id, root_id)?;
    for entry in &entries {
        db::insert_checksum_entry(
            conn,
            db::ChecksumEntryInput {
                collection_id: &collection_id,
                relative_path: &entry.relative_path,
                basename: &entry.basename,
                size_bytes: entry.size_bytes,
                modified_at: entry.modified_at.as_deref(),
                blake3: None,
                sha256: Some(&entry.sha256),
                metadata_json: serde_json::json!({
                    "source": "ssh_native_hash",
                    "root_path": root_path,
                }),
            },
        )?;
        let content_id = db::ensure_content_object_sha256(conn, entry.size_bytes, &entry.sha256)?;
        db::insert_path_observation(
            conn,
            db::PathObservationInput {
                machine_id,
                root_id,
                relative_path: &entry.relative_path,
                basename: &entry.basename,
                parent_path: &entry.parent_path,
                size_bytes: entry.size_bytes,
                modified_at: entry.modified_at.as_deref(),
                content_id: Some(&content_id),
            },
        )?;
    }
    Ok(entries.len() as u64)
}

fn native_hash_ssh_tree(host: &str, remote_path: &str) -> anyhow::Result<Vec<RemoteHashEntry>> {
    let command = format!(
        "find -L {} -type f -printf '%P\\t%s\\t%T+\\t' -exec sha256sum {{}} \\;",
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
        .with_context(|| format!("hashing SSH target {host}:{remote_path}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "ssh native hash failed for {host}:{remote_path}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(parse_native_hash_output(
        remote_path,
        &String::from_utf8_lossy(&output.stdout),
    ))
}

fn parse_native_hash_output(remote_path: &str, stdout: &str) -> Vec<RemoteHashEntry> {
    let mut entries = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let parts = line.splitn(4, '\t').collect::<Vec<_>>();
        if parts.len() != 4 {
            continue;
        }
        let Some(sha256) = parts[3].split_whitespace().next() else {
            continue;
        };
        let relative_path = if parts[0].is_empty() {
            remote_path_basename(remote_path)
        } else {
            parts[0].to_string()
        };
        entries.push(RemoteHashEntry {
            basename: remote_basename(&relative_path).to_string(),
            parent_path: remote_parent(&relative_path),
            relative_path,
            size_bytes: parts[1].parse::<u64>().unwrap_or(0),
            modified_at: Some(parts[2].to_string()),
            sha256: sha256.to_string(),
        });
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    entries
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
}
