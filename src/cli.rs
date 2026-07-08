use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::config::ConfigFormat;
use crate::targets::TargetKind;

#[derive(Debug, Parser)]
#[command(
    name = "gremlin",
    version,
    about = "Local-first file evidence database"
)]
pub struct Cli {
    pub target: Option<String>,
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub no_config: bool,
    #[arg(long, global = true)]
    pub no_tui: bool,
    #[arg(long, global = true)]
    pub machine_label: Option<String>,
    #[arg(long, global = true)]
    pub details: bool,
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true, default_value_t = 20)]
    pub limit: usize,
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Init,
    Scan {
        path: PathBuf,
    },
    Hash {
        path: PathBuf,
        #[arg(long)]
        all: bool,
    },
    ChunkHash {
        target: String,
        #[arg(long, value_enum)]
        kind: Option<TargetKind>,
        #[arg(long, default_value_t = crate::fswork::DEFAULT_CHUNK_SIZE_BYTES / 1024 / 1024)]
        chunk_size_mib: u64,
    },
    Verify {
        target: String,
        #[arg(long)]
        accept: bool,
        #[arg(long, value_enum)]
        kind: Option<TargetKind>,
    },
    Worker {
        #[command(subcommand)]
        command: WorkerCommands,
    },
    ImportEvents {
        input: PathBuf,
        #[arg(long)]
        target: Option<String>,
        #[arg(long, value_enum)]
        kind: Option<TargetKind>,
    },
    ImportManifest {
        input: PathBuf,
    },
    Events,
    Files,
    Jobs,
    Job {
        #[command(subcommand)]
        command: JobCommands,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    Target {
        #[command(subcommand)]
        command: TargetCommands,
    },
    Transfer {
        #[command(subcommand)]
        command: TransferCommands,
    },
    Status {
        target: String,
        #[arg(long, value_enum)]
        kind: Option<TargetKind>,
    },
    Tui,
}

#[derive(Debug, Subcommand)]
pub enum JobCommands {
    Create { kind: JobKind, path: PathBuf },
    Show { job_id: String },
    Run { job_id: String },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum JobKind {
    Scan,
    Hash,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommands {
    Init {
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        default_db: Option<PathBuf>,
        #[arg(long)]
        machine_label: Option<String>,
    },
    Show {
        #[arg(long, value_enum, default_value_t = ConfigFormat::Json)]
        format: ConfigFormat,
    },
    Path,
}

#[derive(Debug, Subcommand)]
pub enum TargetCommands {
    Inspect {
        target: String,
        #[arg(long, value_enum)]
        kind: Option<TargetKind>,
    },
    Add {
        target: String,
        #[arg(long, value_enum)]
        kind: Option<TargetKind>,
        #[arg(long)]
        label: Option<String>,
    },
    Ls {
        target: String,
        #[arg(long, value_enum)]
        kind: Option<TargetKind>,
        #[arg(long, default_value = ".")]
        path: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TransferCommands {
    Plan {
        source: String,
        dest: String,
        #[arg(long, value_enum)]
        source_kind: Option<TargetKind>,
        #[arg(long, value_enum)]
        dest_kind: Option<TargetKind>,
    },
    List,
    Show {
        plan_id: String,
        #[arg(long)]
        action: Option<String>,
    },
    Decide {
        plan_id: String,
        relative_path: String,
        #[arg(long, value_enum)]
        decision: TransferDecision,
        #[arg(long)]
        dest: Option<String>,
    },
    Run {
        plan_id: String,
        #[arg(long)]
        paranoid: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TransferDecision {
    Accept,
    Drop,
    Retarget,
}

impl TransferDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Drop => "drop",
            Self::Retarget => "retarget",
        }
    }

    pub fn action(self) -> &'static str {
        match self {
            Self::Accept => "copy",
            Self::Drop => "skip",
            Self::Retarget => "copy",
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::Accept => "review accepted for copy",
            Self::Drop => "review dropped by user",
            Self::Retarget => "review retargeted for copy",
        }
    }
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Hash => "hash",
        }
    }
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
