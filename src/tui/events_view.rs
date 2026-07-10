use super::*;
pub(super) struct EventsPane<'a> {
    pub(super) events: &'a [db::JobEventRow],
    pub(super) state: &'a AppState,
}

impl Widget for EventsPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let visible = self.events.iter().enumerate().skip(self.state.event_offset);
        let items = if self.events.is_empty() {
            let message = if self.state.event_filter.is_empty() {
                "No activity for this root yet"
            } else {
                "No jobs match the active filter"
            };
            vec![ListItem::new(message)]
        } else {
            let mut rows = vec![ListItem::new(event_header()).style(theme::header())];
            rows.extend(visible.map(|(idx, row)| {
                let marker = if idx == self.state.event_offset {
                    "> "
                } else {
                    "  "
                };
                let style = if idx == self.state.event_offset {
                    theme::selected()
                } else {
                    job_status_style(&event_status(row))
                };
                ListItem::new(event_row(marker, row)).style(style)
            }));
            rows
        };
        List::new(items)
            .style(theme::panel())
            .block(focus_block(
                jobs_title(self.state),
                FocusPane::Events,
                self.state.focus,
            ))
            .render(area, buf);
    }
}

pub(super) fn job_rows(events: &[db::JobEventRow]) -> Vec<db::JobEventRow> {
    let mut seen = BTreeSet::new();
    events
        .iter()
        .filter(|row| seen.insert(row.job_id.clone()))
        .cloned()
        .collect()
}

pub(super) fn filtered_job_rows(rows: &[db::JobEventRow], filter: &str) -> Vec<db::JobEventRow> {
    let needle = filter.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return rows.to_vec();
    }
    rows.iter()
        .filter(|row| event_matches_filter(row, &needle))
        .cloned()
        .collect()
}

fn event_matches_filter(row: &db::JobEventRow, needle: &str) -> bool {
    row.job_id.to_ascii_lowercase().contains(needle)
        || row.job_kind.to_ascii_lowercase().contains(needle)
        || event_status(row).to_ascii_lowercase().contains(needle)
        || event_label(row).to_ascii_lowercase().contains(needle)
        || event_target(row).to_ascii_lowercase().contains(needle)
        || progress_count(row).to_ascii_lowercase().contains(needle)
        || row.payload_json.to_ascii_lowercase().contains(needle)
}

pub(super) fn jobs_title(state: &AppState) -> String {
    if state.event_filter.is_empty() {
        "Jobs".to_string()
    } else if state.event_filter_editing {
        format!("Jobs /{}", state.event_filter)
    } else {
        format!("Jobs filter:{}", state.event_filter)
    }
}

pub(super) fn event_header() -> String {
    format!(
        "{:<2} {:<12} {:<8} {:<10} {:<12} {:<34} {:<36}",
        "", "JOB", "TYPE", "STATUS", "EVENT", "TARGET", "PROGRESS pct done/total @ rate"
    )
}

pub(super) fn event_row(marker: &str, row: &db::JobEventRow) -> String {
    format!(
        "{:<2} {:<12} {:<8} {:<10} {:<12} {:<34} {:<36}",
        marker,
        truncate(short_id(&row.job_id), 12),
        truncate(&row.job_kind, 8),
        truncate(&event_status(row), 10),
        truncate(&event_label(row), 12),
        truncate(&event_target(row), 34),
        truncate(&progress_count(row), 36)
    )
}

fn event_label(row: &db::JobEventRow) -> String {
    if row.event_kind == "job_progress" {
        return row.phase.clone().unwrap_or_else(|| "progress".to_string());
    }
    row.event_kind.clone()
}

fn event_target(row: &db::JobEventRow) -> String {
    if row.job_kind == "transfer_copy" {
        if let Some(direction) = transfer_direction(row.params_json.as_deref()) {
            return direction;
        }
    }
    row.current_path.clone().unwrap_or_else(|| "-".to_string())
}

fn transfer_direction(params_json: Option<&str>) -> Option<String> {
    let payload: serde_json::Value = serde_json::from_str(params_json?).ok()?;
    let source = payload.get("source_path")?.as_str()?;
    let dest = payload.get("dest_path")?.as_str()?;
    Some(format!(
        "{} -> {}",
        display_name_from_path(source),
        display_name_from_path(dest)
    ))
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
        message: payload
            .get("message")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        chunk_confidence: payload
            .get("chunk_confidence")
            .and_then(chunk_confidence_snapshot),
    })
}

fn chunk_confidence_snapshot(value: &serde_json::Value) -> Option<TransferChunkConfidenceSnapshot> {
    Some(TransferChunkConfidenceSnapshot {
        chunks_total: value.get("chunks_total")?.as_u64()?,
        chunks_done: value.get("chunks_done")?.as_u64()?,
        chunks_reused: value.get("chunks_reused")?.as_u64()?,
        chunks_copied: value.get("chunks_copied")?.as_u64()?,
        chunks_verified: value.get("chunks_verified")?.as_u64()?,
        checkpoint_misses: value.get("checkpoint_misses")?.as_u64()?,
    })
}

#[cfg(test)]
pub(super) fn transfer_progress_lines(progress: &TransferProgressSnapshot) -> String {
    let overall_percent = progress_percent(progress.bytes_done, progress.bytes_total);
    let file_percent = progress_percent(progress.file_bytes_done, progress.file_bytes_total);
    let active_file = if progress.files_total == 0 {
        0
    } else {
        progress
            .files_done
            .saturating_add(1)
            .min(progress.files_total)
    };
    let mut lines = format!(
        "Job  {} {:>3}% {}/{} @ {}/s\nFile {} {:>3}% {}/{} ({}/{})\nPath {} | errors {}",
        progress_bar(
            progress.bytes_done,
            progress.bytes_total,
            DETAIL_PROGRESS_WIDTH
        ),
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
        active_file,
        progress.files_total,
        truncate(&progress.current_path, 54),
        progress.errors
    );
    if let Some(message) = progress.message.as_deref() {
        lines.push_str(&format!("\nChunk {}", truncate(message, 72)));
    }
    if let Some(confidence) = progress.chunk_confidence.as_ref() {
        lines.push_str(&format!(
            "\nTrust chunks {}/{} | reused {} | copied {} | verified {} | misses {}",
            confidence.chunks_done,
            confidence.chunks_total,
            confidence.chunks_reused,
            confidence.chunks_copied,
            confidence.chunks_verified,
            confidence.checkpoint_misses
        ));
    }
    lines
}

pub(super) fn transfer_progress_styled_lines(
    progress: &TransferProgressSnapshot,
    phase: usize,
) -> Vec<Line<'static>> {
    let overall_percent = progress_percent(progress.bytes_done, progress.bytes_total);
    let file_percent = progress_percent(progress.file_bytes_done, progress.file_bytes_total);
    let active_file = if progress.files_total == 0 {
        0
    } else {
        progress
            .files_done
            .saturating_add(1)
            .min(progress.files_total)
    };
    let mut lines = vec![
        progress_detail_line(
            "Job ",
            progress.bytes_done,
            progress.bytes_total,
            phase,
            format!(
                " {:>3}% {}/{} @ {}/s",
                overall_percent,
                human_size(progress.bytes_done),
                human_size(progress.bytes_total),
                transfer_rate(progress.bytes_per_second)
            ),
        ),
        progress_detail_line(
            "File",
            progress.file_bytes_done,
            progress.file_bytes_total,
            phase + 2,
            format!(
                " {:>3}% {}/{} ({}/{})",
                file_percent,
                human_size(progress.file_bytes_done),
                human_size(progress.file_bytes_total),
                active_file,
                progress.files_total
            ),
        ),
        Line::from(format!(
            "Path {} | errors {}",
            truncate(&progress.current_path, 54),
            progress.errors
        )),
    ];
    if let Some(message) = progress.message.as_deref() {
        lines.push(Line::from(format!("Chunk {}", truncate(message, 72))));
    }
    if let Some(confidence) = progress.chunk_confidence.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("Trust ", theme::header()),
            Span::raw(format!(
                "chunks {}/{} | reused {} | copied {} | verified {} | misses {}",
                confidence.chunks_done,
                confidence.chunks_total,
                confidence.chunks_reused,
                confidence.chunks_copied,
                confidence.chunks_verified,
                confidence.checkpoint_misses
            )),
        ]));
    }
    lines
}

fn progress_detail_line(
    label: &'static str,
    done: u64,
    total: u64,
    phase: usize,
    suffix: String,
) -> Line<'static> {
    let mut spans = vec![Span::raw(format!("{label} "))];
    spans.extend(animated_progress_bar_spans(
        done,
        total,
        DETAIL_PROGRESS_WIDTH,
        phase,
    ));
    spans.push(Span::raw(suffix));
    Line::from(spans)
}

pub(super) fn animated_progress_bar_spans(
    done: u64,
    total: u64,
    width: usize,
    phase: usize,
) -> Vec<Span<'static>> {
    if width == 0 {
        return vec![Span::styled("[]", theme::muted())];
    }
    let clamped_done = done.min(total);
    let eighths_total = if total == 0 {
        0
    } else {
        ((clamped_done as u128) * (width as u128) * 8) / (total as u128)
    };
    let full = (eighths_total / 8).min(width as u128) as usize;
    let partial = (eighths_total % 8) as usize;
    let mut spans = Vec::with_capacity(width + 2);
    spans.push(Span::styled("▕", theme::muted()));
    for idx in 0..full {
        spans.push(progress_cell("▌", idx, phase, true));
    }
    if full < width {
        if partial > 0 {
            spans.push(progress_cell(PARTIAL_BLOCKS[partial], full, phase, true));
        }
        let empty = width.saturating_sub(full + usize::from(partial > 0));
        for idx in 0..empty {
            spans.push(progress_cell(
                "░",
                full + usize::from(partial > 0) + idx,
                phase,
                false,
            ));
        }
    }
    spans.push(Span::styled("▏", theme::muted()));
    spans
}

fn progress_cell(symbol: &'static str, idx: usize, phase: usize, filled: bool) -> Span<'static> {
    let fg = progress_wave_color((idx * 2) as f64, phase, 0.0);
    let bg = progress_wave_color((idx * 2 + 1) as f64, phase, 0.0);
    if filled {
        Span::styled(symbol, Style::default().fg(fg).bg(bg))
    } else {
        Span::styled(symbol, Style::default().fg(bg).bg(theme::PANEL_DARK))
    }
}

fn progress_wave_color(sample: f64, phase: usize, offset: f64) -> ratatui::style::Color {
    let wave = ((sample * 0.34) + (phase as f64 * 0.22) + offset).sin();
    let normalized = (wave + 1.0) * 0.5;
    interpolated_gradient_color(normalized)
}

fn interpolated_gradient_color(position: f64) -> ratatui::style::Color {
    let gradient = theme::PROGRESS_GRADIENT;
    let scaled = position.clamp(0.0, 1.0) * (gradient.len().saturating_sub(1) as f64);
    let left = scaled.floor() as usize;
    let right = (left + 1).min(gradient.len() - 1);
    let amount = scaled - (left as f64);
    let (left_r, left_g, left_b) = color_rgb(gradient[left]);
    let (right_r, right_g, right_b) = color_rgb(gradient[right]);
    ratatui::style::Color::Rgb(
        interpolate_channel(left_r, right_r, amount),
        interpolate_channel(left_g, right_g, amount),
        interpolate_channel(left_b, right_b, amount),
    )
}

fn color_rgb(color: ratatui::style::Color) -> (u8, u8, u8) {
    match color {
        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
        _ => (0xff, 0xff, 0xff),
    }
}

fn interpolate_channel(left: u8, right: u8, amount: f64) -> u8 {
    (left as f64 + ((right as f64 - left as f64) * amount)).round() as u8
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
        "{} {:>3}% {}/{} @ {}/s",
        progress_bar(done, total, EVENT_PROGRESS_WIDTH),
        progress_percent(done, total),
        human_size(done),
        human_size(total),
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
    TextProgressBar { done, total, width }.label()
}

pub(super) struct TextProgressBar {
    pub(super) done: u64,
    pub(super) total: u64,
    pub(super) width: usize,
}

impl TextProgressBar {
    pub(super) fn label(&self) -> String {
        if self.width == 0 {
            return "[]".to_string();
        }
        let clamped_done = self.done.min(self.total);
        let eighths_total = if self.total == 0 {
            0
        } else {
            ((clamped_done as u128) * (self.width as u128) * 8) / (self.total as u128)
        };
        let full = (eighths_total / 8).min(self.width as u128) as usize;
        let partial = (eighths_total % 8) as usize;
        let mut bar = String::with_capacity(self.width + 2);
        bar.push('▕');
        bar.push_str(&"█".repeat(full));
        if full < self.width {
            bar.push_str(PARTIAL_BLOCKS[partial]);
            let empty = self.width.saturating_sub(full + usize::from(partial > 0));
            bar.push_str(&"░".repeat(empty));
        }
        bar.push('▏');
        bar
    }
}

impl Widget for TextProgressBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.label())
            .style(theme::panel())
            .render(area, buf);
    }
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
