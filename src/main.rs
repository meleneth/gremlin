mod cli;
mod config;
mod db;
mod error;
mod events;
mod fswork;
mod import;
mod tui;
mod util;

use anyhow::Context;
use clap::Parser;
use cli::{Cli, Commands, ConfigCommands, JobCommands, WorkerCommands};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config_ctx = config::load(cli.config.clone(), cli.no_config)?;
    let machine_label = config_ctx.machine_label(cli.machine_label.clone());

    match cli.command {
        Commands::Init => {
            let db = config_ctx.resolve_db(cli.db.clone())?;
            let conn =
                db::open_or_create(&db).with_context(|| format!("opening {}", db.display()))?;
            db::init_schema(&conn)?;
            println!("initialized {}", db.display());
        }
        Commands::Scan { path } => {
            let db = config_ctx.resolve_db(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            fswork::scan_to_db(&conn, &path, &db, machine_label.as_deref())?;
        }
        Commands::Hash { path } => {
            let db = config_ctx.resolve_db(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            fswork::hash_to_db(&conn, &path, &db, machine_label.as_deref())?;
        }
        Commands::Worker { command } => match command {
            WorkerCommands::Hash { path, jsonl, out } => {
                if !jsonl {
                    anyhow::bail!("worker hash currently requires --jsonl");
                }
                fswork::worker_hash_jsonl(&path, out.as_deref())?;
            }
        },
        Commands::ImportEvents { input } => {
            let db = config_ctx.resolve_db(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            import::import_events_file(&conn, &input)?;
        }
        Commands::Events => {
            let db = config_ctx.resolve_db(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            for row in db::recent_events(&conn, config_ctx.jobs_limit())? {
                println!(
                    "{} {} #{} {} {}",
                    row.created_at, row.job_id, row.sequence, row.event_kind, row.payload_json
                );
            }
        }
        Commands::Files => {
            let db = config_ctx.resolve_db(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            for row in db::recent_files(&conn, config_ctx.jobs_limit())? {
                println!(
                    "{}\t{}\t{}\t{}",
                    row.size_bytes,
                    row.status,
                    row.content_id.unwrap_or_else(|| "-".to_string()),
                    row.relative_path
                );
            }
        }
        Commands::Jobs => {
            let db = config_ctx.resolve_db(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            for row in db::recent_jobs(&conn, config_ctx.jobs_limit())? {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    row.id,
                    row.kind,
                    row.status,
                    row.created_at,
                    row.params_json.unwrap_or_else(|| "{}".to_string())
                );
            }
        }
        Commands::Job { command } => match command {
            JobCommands::Create { kind, path } => {
                let db = config_ctx.resolve_db(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                let job_id =
                    db::queue_file_job(&conn, kind.as_str(), &path, machine_label.as_deref())?;
                println!("queued {} job {job_id}", kind.as_str());
            }
            JobCommands::Show { job_id } => {
                let db = config_ctx.resolve_db(cli.db.clone())?;
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
                let db = config_ctx.resolve_db(cli.db.clone())?;
                let conn = db::open_existing(&db)?;
                fswork::run_queued_job(&conn, &job_id, &db, machine_label.as_deref())?;
            }
        },
        Commands::Config { command } => match command {
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
        Commands::Tui => {
            let db = config_ctx.resolve_db(cli.db.clone())?;
            let conn = db::open_existing(&db)?;
            tui::run_with_options(&conn, machine_label)?;
        }
    }

    Ok(())
}
