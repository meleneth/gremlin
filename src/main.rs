mod cli;
mod db;
mod error;
mod events;
mod fswork;
mod import;
mod tui;
mod util;

use anyhow::Context;
use clap::Parser;
use cli::{Cli, Commands, JobCommands, WorkerCommands};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { db } => {
            let conn =
                db::open_or_create(&db).with_context(|| format!("opening {}", db.display()))?;
            db::init_schema(&conn)?;
            println!("initialized {}", db.display());
        }
        Commands::Scan { path, db } => {
            let conn = db::open_existing(&db)?;
            fswork::scan_to_db(&conn, &path, &db)?;
        }
        Commands::Hash { path, db } => {
            let conn = db::open_existing(&db)?;
            fswork::hash_to_db(&conn, &path, &db)?;
        }
        Commands::Worker { command } => match command {
            WorkerCommands::Hash { path, jsonl, out } => {
                if !jsonl {
                    anyhow::bail!("worker hash currently requires --jsonl");
                }
                fswork::worker_hash_jsonl(&path, out.as_deref())?;
            }
        },
        Commands::ImportEvents { input, db } => {
            let conn = db::open_existing(&db)?;
            import::import_events_file(&conn, &input)?;
        }
        Commands::Events { db } => {
            let conn = db::open_existing(&db)?;
            for row in db::recent_events(&conn, 200)? {
                println!(
                    "{} {} #{} {} {}",
                    row.created_at, row.job_id, row.sequence, row.event_kind, row.payload_json
                );
            }
        }
        Commands::Files { db } => {
            let conn = db::open_existing(&db)?;
            for row in db::recent_files(&conn, 200)? {
                println!(
                    "{}\t{}\t{}\t{}",
                    row.size_bytes,
                    row.status,
                    row.content_id.unwrap_or_else(|| "-".to_string()),
                    row.relative_path
                );
            }
        }
        Commands::Jobs { db } => {
            let conn = db::open_existing(&db)?;
            for row in db::recent_jobs(&conn, 200)? {
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
            JobCommands::Create { kind, path, db } => {
                let conn = db::open_existing(&db)?;
                let job_id = db::queue_file_job(&conn, kind.as_str(), &path)?;
                println!("queued {} job {job_id}", kind.as_str());
            }
            JobCommands::Show { job_id, db } => {
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
        },
        Commands::Tui { db } => {
            let conn = db::open_existing(&db)?;
            tui::run(&conn)?;
        }
    }

    Ok(())
}
