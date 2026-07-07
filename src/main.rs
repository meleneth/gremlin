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
use cli::{Cli, Commands, WorkerCommands};

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
        Commands::Tui { db } => {
            let conn = db::open_existing(&db)?;
            tui::run(&conn)?;
        }
    }

    Ok(())
}
