use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "gremlin",
    version,
    about = "Local-first file evidence database"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Init {
        #[arg(long)]
        db: PathBuf,
    },
    Scan {
        path: PathBuf,
        #[arg(long)]
        db: PathBuf,
    },
    Hash {
        path: PathBuf,
        #[arg(long)]
        db: PathBuf,
    },
    Worker {
        #[command(subcommand)]
        command: WorkerCommands,
    },
    ImportEvents {
        input: PathBuf,
        #[arg(long)]
        db: PathBuf,
    },
    Events {
        #[arg(long)]
        db: PathBuf,
    },
    Files {
        #[arg(long)]
        db: PathBuf,
    },
    Tui {
        #[arg(long)]
        db: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorkerCommands {
    Hash {
        path: PathBuf,
        #[arg(long)]
        jsonl: bool,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}
