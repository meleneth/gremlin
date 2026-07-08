use super::*;
pub(super) fn render_events(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    events: &[db::JobEventRow],
    state: &AppState,
) {
    let visible = events.iter().enumerate().skip(state.event_offset);
    let items = if events.is_empty() {
        vec![ListItem::new("No jobs or events for this root")]
    } else {
        let mut rows = vec![ListItem::new(event_header()).style(theme::header())];
        rows.extend(visible.map(|(idx, row)| {
            let marker = if idx == state.event_offset {
                "> "
            } else {
                "  "
            };
            let style = if idx == state.event_offset {
                theme::selected()
            } else {
                job_status_style(&event_status(row))
            };
            ListItem::new(event_row(marker, row)).style(style)
        }));
        rows
    };
    frame.render_widget(
        List::new(items).style(theme::panel()).block(focus_block(
            "Jobs",
            FocusPane::Events,
            state.focus,
        )),
        area,
    );
}

pub(super) fn event_header() -> String {
    format!(
        "{:<2} {:<18} {:<5} {:<9} {:<10} {:<24}",
        "", "JOB", "KIND", "STATUS", "PHASE", "PROGRESS"
    )
}

pub(super) fn event_row(marker: &str, row: &db::JobEventRow) -> String {
    format!(
        "{:<2} {:<18} {:<5} {:<9} {:<10} {:<24}",
        marker,
        short_id(&row.job_id),
        truncate(&row.job_kind, 5),
        truncate(&event_status(row), 9),
        truncate(row.phase.as_deref().unwrap_or("-"), 10),
        truncate(&progress_count(row), 24)
    )
}

pub(super) fn event_summary(row: &db::JobEventRow) -> String {
    format!(
        "{} {} #{} {} {} {}",
        row.job_kind,
        event_status(row),
        row.sequence,
        row.event_kind,
        progress_count(row),
        truncate(&row.payload_json, 28)
    )
}

pub(super) fn event_status(row: &db::JobEventRow) -> String {
    if row.cancel_requested && matches!(row.status.as_str(), "created" | "running") {
        "canceling".to_string()
    } else {
        row.status.clone()
    }
}

pub(super) fn progress_count(row: &db::JobEventRow) -> String {
    if let Some(progress) = byte_progress_summary(&row.payload_json) {
        return progress;
    }
    if row.files_skipped > 0 || row.errors > 0 {
        format!("{}/{}/{}", row.files_done, row.files_skipped, row.errors)
    } else {
        format!("{}/{}", row.files_done, row.files_seen)
    }
}

pub(super) fn latest_transfer_progress(
    events: &[db::JobEventRow],
) -> Option<TransferProgressSnapshot> {
    events
        .iter()
        .filter(|row| row.job_kind == "transfer_copy")
        .find_map(|row| transfer_progress_snapshot(&row.payload_json))
}

pub(super) fn transfer_progress_snapshot(payload_json: &str) -> Option<TransferProgressSnapshot> {
    let payload: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    if payload.get("type")?.as_str()? != "job_progress" {
        return None;
    }
    Some(TransferProgressSnapshot {
        current_path: payload
            .get("current_path")
            .and_then(|value| value.as_str())
            .unwrap_or("-")
            .to_string(),
        files_done: payload
            .get("files_done")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        files_total: payload
            .get("files_total")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        bytes_done: payload.get("bytes_done")?.as_u64()?,
        bytes_total: payload.get("bytes_total")?.as_u64()?,
        file_bytes_done: payload
            .get("file_bytes_done")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        file_bytes_total: payload
            .get("file_bytes_total")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        bytes_per_second: payload
            .get("bytes_per_second")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0),
        errors: payload
            .get("errors")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
    })
}

pub(super) fn transfer_progress_lines(progress: &TransferProgressSnapshot) -> String {
    let overall_percent = progress_percent(progress.bytes_done, progress.bytes_total);
    let file_percent = progress_percent(progress.file_bytes_done, progress.file_bytes_total);
    format!(
        "Overall {} {:>3}% {}/{} @ {}/s\nCurrent {} {:>3}% {}/{}\nNow: {} | files {}/{} | errors {}",
        progress_bar(progress.bytes_done, progress.bytes_total, DETAIL_PROGRESS_WIDTH),
        overall_percent,
        human_size(progress.bytes_done),
        human_size(progress.bytes_total),
        transfer_rate(progress.bytes_per_second),
        progress_bar(
            progress.file_bytes_done,
            progress.file_bytes_total,
            DETAIL_PROGRESS_WIDTH
        ),
        file_percent,
        human_size(progress.file_bytes_done),
        human_size(progress.file_bytes_total),
        truncate(&progress.current_path, 36),
        progress.files_done,
        progress.files_total,
        progress.errors
    )
}

pub(super) fn byte_progress_summary(payload_json: &str) -> Option<String> {
    let payload: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    if payload.get("type")?.as_str()? != "job_progress" {
        return None;
    }
    let done = payload.get("bytes_done")?.as_u64()?;
    let total = payload.get("bytes_total")?.as_u64()?;
    if total == 0 {
        return None;
    }
    let rate = payload
        .get("bytes_per_second")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    Some(format!(
        "{} {:>3}% {:>7}/s",
        progress_bar(done, total, EVENT_PROGRESS_WIDTH),
        ((done.saturating_mul(100)) / total).min(100),
        transfer_rate(rate)
    ))
}

pub(super) fn progress_percent(done: u64, total: u64) -> u64 {
    done.min(total)
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100)
}

const DETAIL_PROGRESS_WIDTH: usize = 28;
const EVENT_PROGRESS_WIDTH: usize = 14;
const PARTIAL_BLOCKS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

pub(super) fn progress_bar(done: u64, total: u64, width: usize) -> String {
    if width == 0 {
        return "[]".to_string();
    }
    let clamped_done = done.min(total);
    let eighths_total = if total == 0 {
        0
    } else {
        ((clamped_done as u128) * (width as u128) * 8) / (total as u128)
    };
    let full = (eighths_total / 8).min(width as u128) as usize;
    let partial = (eighths_total % 8) as usize;
    let mut bar = String::with_capacity(width + 2);
    bar.push('▕');
    bar.push_str(&"█".repeat(full));
    if full < width {
        bar.push_str(PARTIAL_BLOCKS[partial]);
        let empty = width.saturating_sub(full + usize::from(partial > 0));
        bar.push_str(&"░".repeat(empty));
    }
    bar.push('▏');
    bar
}

pub(super) fn transfer_rate(bytes_per_second: f64) -> String {
    if bytes_per_second >= 1024.0 * 1024.0 {
        format!("{:.1} MiB", bytes_per_second / 1024.0 / 1024.0)
    } else if bytes_per_second >= 1024.0 {
        format!("{:.1} KiB", bytes_per_second / 1024.0)
    } else {
        format!("{:.0} B", bytes_per_second)
    }
}
