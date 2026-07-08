use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Widget, Wrap};
use ratatui::Terminal;
use rusqlite::Connection;
use tokio::sync::mpsc;
use tokio::task;

use crate::collections;
use crate::db;
use crate::fswork::{self, OutputOptions};
use crate::transfer;
use crate::util::human_size;

const DETAIL_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Default)]
struct AppState {
    focus: FocusPane,
    file_view: FileView,
    selected_root: usize,
    file_offset: usize,
    file_filter: String,
    file_filter_editing: bool,
    detail_file_offset: usize,
    detail_pending_file_offset: usize,
    detail_selection_key: Option<String>,
    detail_selection_changed_at: Option<Instant>,
    plan_offset: usize,
    event_offset: usize,
    status: String,
    transfer_source_root_id: Option<String>,
    transfer_run_plan_id: Option<String>,
    retarget_draft: Option<RetargetDraft>,
    pending_delete_root_id: Option<String>,
    pending_import: Option<PendingTemporaryImport>,
    pending_scoped_job: Option<PendingScopedJob>,
    active_background_jobs: usize,
    activities: VecDeque<ActivityMessage>,
    last_plan: Option<PlanSnapshot>,
    collection_result: Option<CollectionSnapshot>,
    temporary_browse: Option<TemporaryBrowse>,
    root_browse_dirs: BTreeMap<String, String>,
}

impl AppState {
    fn sync_detail_selection(&mut self, selection_key: String, file_count: usize, now: Instant) {
        if self.detail_selection_key.as_deref() != Some(selection_key.as_str()) {
            self.detail_selection_key = Some(selection_key);
            self.detail_file_offset = self.file_offset.min(file_count.saturating_sub(1));
            self.detail_pending_file_offset = self.detail_file_offset;
            self.detail_selection_changed_at = None;
            return;
        }

        let current = self.file_offset.min(file_count.saturating_sub(1));
        if current == self.detail_file_offset {
            self.detail_pending_file_offset = current;
            self.detail_selection_changed_at = None;
            return;
        }

        if current != self.detail_pending_file_offset {
            self.detail_pending_file_offset = current;
            self.detail_selection_changed_at = Some(now);
            return;
        }

        if self
            .detail_selection_changed_at
            .is_some_and(|changed_at| now.duration_since(changed_at) >= DETAIL_DEBOUNCE)
        {
            self.detail_file_offset = current;
            self.detail_selection_changed_at = None;
        }
    }

    fn set_status(&mut self, level: ActivityLevel, message: impl Into<String>) {
        let message = message.into();
        self.status = message.clone();
        self.activities
            .push_back(ActivityMessage { level, message });
        while self.activities.len() > 50 {
            self.activities.pop_front();
        }
    }

    fn background_started(&mut self, message: impl Into<String>) {
        self.active_background_jobs += 1;
        self.set_status(ActivityLevel::Info, message);
    }

    fn background_finished(&mut self, level: ActivityLevel, message: impl Into<String>) {
        self.active_background_jobs = self.active_background_jobs.saturating_sub(1);
        self.set_status(level, message);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl ActivityLevel {
    fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "ok",
            Self::Warning => "warn",
            Self::Error => "err",
        }
    }

    fn style(self) -> Style {
        match self {
            Self::Info => theme::muted(),
            Self::Success => theme::ok(),
            Self::Warning => theme::warn(),
            Self::Error => theme::error(),
        }
    }
}

#[derive(Debug, Clone)]
struct ActivityMessage {
    level: ActivityLevel,
    message: String,
}

pub type BrowseProvider =
    Arc<dyn Fn(&str) -> anyhow::Result<Vec<InitialBrowseEntry>> + Send + Sync + 'static>;
pub type ImportProvider =
    Arc<dyn Fn(ImportMode, &str) -> anyhow::Result<ImportResult> + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    No,
    Fast,
    Hash,
}

#[derive(Debug, Clone)]
pub struct ImportResult {
    pub mode: ImportMode,
    pub root_id: String,
    pub root_path: String,
    pub files_imported: u64,
}

#[derive(Clone)]
pub struct InitialBrowse {
    pub label: String,
    pub machine_id: String,
    pub root_path: String,
    pub current_path: String,
    pub entries: Vec<InitialBrowseEntry>,
    pub browse_provider: Option<BrowseProvider>,
    pub import_provider: Option<ImportProvider>,
}

#[derive(Debug, Clone)]
pub struct InitialBrowseEntry {
    pub kind: String,
    pub name: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
}

#[derive(Clone)]
struct TemporaryBrowse {
    label: String,
    machine_id: String,
    root_path: String,
    current_path: String,
    entries: Vec<InitialBrowseEntry>,
    browse_provider: Option<BrowseProvider>,
    import_provider: Option<ImportProvider>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    File,
    Directory,
}

#[derive(Debug, Clone)]
struct FileViewRow {
    relative_path: String,
    size_bytes: i64,
    modified_at: Option<String>,
    content_id: Option<String>,
    status: String,
    kind: FileKind,
}

impl From<InitialBrowse> for TemporaryBrowse {
    fn from(value: InitialBrowse) -> Self {
        Self {
            label: value.label,
            machine_id: value.machine_id,
            root_path: value.root_path,
            current_path: value.current_path,
            entries: value.entries,
            browse_provider: value.browse_provider,
            import_provider: value.import_provider,
        }
    }
}

impl From<&db::FileRow> for FileViewRow {
    fn from(value: &db::FileRow) -> Self {
        Self {
            relative_path: value.relative_path.clone(),
            size_bytes: value.size_bytes,
            modified_at: value.modified_at.clone(),
            content_id: value.content_id.clone(),
            status: value.status.clone(),
            kind: FileKind::File,
        }
    }
}

impl FileViewRow {
    fn from_cached_directory_entry(entry: &db::CachedDirectoryEntry) -> Self {
        let kind = if entry.kind == "dir" {
            FileKind::Directory
        } else {
            FileKind::File
        };
        Self {
            relative_path: entry.relative_path.clone(),
            size_bytes: entry.size_bytes,
            modified_at: entry.modified_at.clone(),
            content_id: entry.content_id.clone(),
            status: entry.status.clone().unwrap_or_else(|| {
                if kind == FileKind::Directory {
                    format!("dir:{}", entry.file_count)
                } else {
                    "present".to_string()
                }
            }),
            kind,
        }
    }

    fn from_temporary_entry(entry: &InitialBrowseEntry) -> Self {
        let kind = if entry.kind == "dir" {
            FileKind::Directory
        } else {
            FileKind::File
        };
        Self {
            relative_path: entry.name.clone(),
            size_bytes: entry.size_bytes as i64,
            modified_at: entry.modified_at.clone(),
            content_id: None,
            status: if kind == FileKind::Directory {
                "dir".to_string()
            } else {
                "remote".to_string()
            },
            kind,
        }
    }
}

#[derive(Debug, Clone)]
struct PlanSnapshot {
    plan_id: String,
    source_root_id: String,
    status: String,
    source_name: String,
    dest_name: String,
    summary: Vec<db::TransferPlanActionSummary>,
    entries: Vec<db::TransferPlanEntryRow>,
}

#[derive(Debug, Clone)]
struct CollectionSnapshot {
    collection_id: String,
    collection_name: String,
    root_id: String,
    root_path: String,
    entries: usize,
    ok: usize,
    size_only: usize,
    missing: usize,
    size_mismatch: usize,
    hash_mismatch: usize,
    unverified: usize,
    extras: usize,
    rows: Vec<CollectionResultRow>,
}

#[derive(Debug, Clone)]
struct CollectionResultRow {
    kind: String,
    relative_path: String,
    expected_size_bytes: u64,
    actual_size_bytes: Option<u64>,
}

impl From<collections::CollectionVerifySummary> for CollectionSnapshot {
    fn from(value: collections::CollectionVerifySummary) -> Self {
        let mut rows = value
            .findings
            .into_iter()
            .map(|finding| CollectionResultRow {
                kind: finding.kind.as_str().to_string(),
                relative_path: finding.relative_path,
                expected_size_bytes: finding.expected_size_bytes,
                actual_size_bytes: finding.actual_size_bytes,
            })
            .collect::<Vec<_>>();
        rows.extend(
            value
                .extra_files
                .into_iter()
                .map(|extra| CollectionResultRow {
                    kind: "extra".to_string(),
                    relative_path: extra.relative_path,
                    expected_size_bytes: 0,
                    actual_size_bytes: Some(extra.size_bytes),
                }),
        );
        Self {
            collection_id: value.collection_id,
            collection_name: value.collection_name,
            root_id: value.root_id,
            root_path: value.root_path,
            entries: value.entries,
            ok: value.ok,
            size_only: value.size_only,
            missing: value.missing,
            size_mismatch: value.size_mismatch,
            hash_mismatch: value.hash_mismatch,
            unverified: value.unverified,
            extras: value.extras,
            rows,
        }
    }
}

#[derive(Debug)]
enum TuiMessage {
    Status(String),
    TransferFinished {
        plan_id: String,
        status: String,
    },
    ImportFinished(String),
    TemporaryTransferSourceImported {
        root_id: String,
        selected_relative_path: Option<String>,
        mark_all: bool,
        status: String,
    },
}

struct InfoBarData<'a> {
    root_name: Option<String>,
    file: Option<&'a FileViewRow>,
    selection: Option<&'a db::SelectionSummary>,
    event: Option<&'a db::JobEventRow>,
    root_count: usize,
}

struct DetailData<'a> {
    root: Option<&'a db::RootRow>,
    temporary_browse: Option<&'a TemporaryBrowse>,
    persisted_browse_dir: Option<&'a str>,
    summary: Option<&'a db::RootSummary>,
    selection: Option<&'a db::SelectionSummary>,
    file: Option<&'a FileViewRow>,
    selected_paths: &'a BTreeSet<String>,
    plan: Option<&'a PlanSnapshot>,
    collection: Option<&'a CollectionSnapshot>,
    transfer_progress: Option<TransferProgressSnapshot>,
}

#[derive(Debug, Clone)]
struct TransferProgressSnapshot {
    current_path: String,
    files_done: u64,
    files_total: u64,
    bytes_done: u64,
    bytes_total: u64,
    file_bytes_done: u64,
    file_bytes_total: u64,
    bytes_per_second: f64,
    errors: u64,
    message: Option<String>,
}

#[derive(Debug, Clone)]
struct RetargetDraft {
    plan_id: String,
    relative_path: String,
    value: String,
}

#[derive(Debug, Clone)]
struct PendingTemporaryImport {
    remote_path: String,
}

#[derive(Debug, Clone)]
struct PendingScopedJob {
    kind: String,
    root_id: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FocusPane {
    #[default]
    Roots,
    Files,
    Plan,
    Events,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FileView {
    #[default]
    Basic,
    Meta,
    Hash,
    All,
}

impl FileView {
    fn next(self) -> Self {
        match self {
            Self::Basic => Self::Meta,
            Self::Meta => Self::Hash,
            Self::Hash => Self::All,
            Self::All => Self::Basic,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Meta => "meta",
            Self::Hash => "hash",
            Self::All => "all",
        }
    }
}

impl FocusPane {
    fn next(self) -> Self {
        match self {
            Self::Roots => Self::Files,
            Self::Files => Self::Plan,
            Self::Plan => Self::Events,
            Self::Events => Self::Roots,
        }
    }

    fn title(self, title: &str, active: Self) -> String {
        if self == active {
            format!("{title} *")
        } else {
            title.to_string()
        }
    }
}

fn panel_block(title: &'static str, active: bool) -> Block<'static> {
    let border = if active {
        theme::BORDER_ACTIVE
    } else {
        theme::BORDER
    };
    let title_style = if active {
        theme::active_title()
    } else {
        theme::inactive_title()
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(theme::panel())
        .border_style(Style::default().fg(border).bg(theme::PANEL))
        .title_style(title_style)
}

fn focus_block(title: impl Into<String>, pane: FocusPane, active: FocusPane) -> Block<'static> {
    let focused = pane == active;
    let title = title.into();
    let border = if focused {
        theme::BORDER_ACTIVE
    } else {
        theme::BORDER
    };
    let title_style = if focused {
        theme::active_title()
    } else {
        theme::inactive_title()
    };
    Block::default()
        .title(pane.title(&title, active))
        .borders(Borders::ALL)
        .style(theme::panel())
        .border_style(Style::default().fg(border).bg(theme::PANEL))
        .title_style(title_style)
}

fn attention_focus_block(
    title: impl Into<String>,
    pane: FocusPane,
    active: FocusPane,
    attention: bool,
) -> Block<'static> {
    let title = title.into();
    if !attention {
        return focus_block(title, pane, active);
    }
    Block::default()
        .title(pane.title(&title, active))
        .borders(Borders::ALL)
        .style(theme::attention())
        .border_style(Style::default().fg(theme::ACCENT).bg(theme::ATTENTION))
        .title_style(Style::default().fg(theme::TEXT).bg(theme::ATTENTION))
}

fn file_status_style(status: &str) -> Style {
    match status {
        "present" => theme::panel(),
        "missing" => theme::warn(),
        "error" => theme::error(),
        _ => theme::muted(),
    }
}

fn job_status_style(status: &str) -> Style {
    match status {
        "completed" => theme::ok(),
        "created" | "running" | "canceling" => Style::default().fg(theme::BLUE).bg(theme::PANEL),
        "completed_with_errors" | "canceled" => theme::warn(),
        "failed" => theme::error(),
        _ => theme::muted(),
    }
}

mod app;
mod collection_actions;
mod detail;
mod events_view;
mod files;
mod hints;
mod import_actions;
mod jobs;
mod navigation;
mod plan_actions;
mod plan_view;
mod root_actions;
mod roots;
mod screen;
mod theme;
mod transfer_source;

pub use app::{run_with_initial_browse, run_with_options};

use collection_actions::*;
use detail::*;
use events_view::*;
use files::*;
use hints::*;
use import_actions::*;
use jobs::*;
use navigation::*;
use plan_actions::*;
use plan_view::*;
use root_actions::*;
use roots::*;
use screen::*;
use transfer_source::*;

#[cfg(test)]
mod tests;
