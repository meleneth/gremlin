mod cli;
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
    };

    match cli.command {
        None => {
            let Some(target) = cli.target.as_deref() else {
                let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
                let conn = db::open_or_create(&db)?;
                db::init_schema(&conn)?;
                tui::run_with_options(&conn, &db, machine_label).await?;
                return Ok(());
            };
            run_default_target(
                &config_ctx,
                cli.db.clone(),
                target,
                machine_label.as_deref(),
                output,
            )?;
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
        Some(Commands::Worker { command }) => match command {
            WorkerCommands::Hash { path, jsonl, out } => {
                if !jsonl {
                    anyhow::bail!("worker hash currently requires --jsonl");
                }
                fswork::worker_hash_jsonl(&path, out.as_deref())?;
            }
        },
        Some(Commands::ImportEvents { input }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            import::import_events_file(&conn, &input)?;
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
            }
        },
        Some(Commands::Status { target, kind }) => {
            let db = config_ctx.resolve_db_or_default(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            let parsed = targets::parse_target(&target, kind)?;
            let (machine_id, root_path) =
                resolve_target_identity(&conn, &parsed, machine_label.as_deref())?;
            match db::target_status(&conn, &machine_id, &root_path)? {
                Some(status) => print_target_status(&parsed, status),
                None => {
                    println!("target:\t{}", parsed.original);
                    println!("kind:\t{:?}", parsed.kind);
                    println!("machine:\t{}", parsed.display_machine_label());
                    println!("path:\t{}", root_path);
                    println!("known:\tno");
                    println!("next:\tgremlin target add {} --db <db>", parsed.original);
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

fn run_default_target(
    config_ctx: &config::ConfigContext,
    cli_db: Option<std::path::PathBuf>,
    target: &str,
    machine_label: Option<&str>,
    output: fswork::OutputOptions,
) -> anyhow::Result<()> {
    let db_path = config_ctx.resolve_db_or_default(cli_db)?;
    let conn = db::open_or_create(&db_path)?;
    db::init_schema(&conn)?;
    let parsed = targets::parse_target(target, None)?;
    let (machine_id, root_path) = resolve_target_identity(&conn, &parsed, machine_label)?;
    let root_id = db::ensure_root(&conn, &machine_id, &root_path)?;
    match parsed.kind {
        TargetKind::LocalPath | TargetKind::FileUrl => {
            println!("db:\t{}", db_path.display());
            println!("target:\t{}", parsed.original);
            println!("root:\t{root_id}");
            let local_path = parsed
                .local_path()
                .ok_or_else(|| anyhow::anyhow!("target is not local file-like"))?;
            fswork::scan_to_db(&conn, &local_path, &db_path, machine_label, output)?;
            if let Some(status) = db::target_status(&conn, &machine_id, &root_path)? {
                print_target_status(&parsed, status);
            }
            println!(
                "next:\tgremlin hash {} --db {}",
                parsed.original,
                db_path.display()
            );
        }
        TargetKind::Ssh | TargetKind::Url => {
            println!("db:\t{}", db_path.display());
            println!(
                "target {:?}\tmachine={}\troot={}\tpath={}",
                parsed.kind, machine_id, root_id, root_path
            );
            match db::target_status(&conn, &machine_id, &root_path)? {
                Some(status) => print_target_status(&parsed, status),
                None => println!("known:\tregistered"),
            }
            println!("next:\tremote worker/import is not implemented yet");
        }
    }
    Ok(())
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

fn print_target_status(parsed: &ParsedTarget, status: db::TargetStatus) {
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
        "entry:\t{}\t{}\t{}\t{}\tsource={}\tdest={}\tmetadata={}",
        entry.action,
        util::human_size(entry.size_bytes),
        entry.reason,
        entry.relative_path,
        entry.source_content_id.as_deref().unwrap_or("-"),
        entry.dest_content_id.as_deref().unwrap_or("-"),
        entry.metadata_json
    );
}
