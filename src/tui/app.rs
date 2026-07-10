use super::*;
pub async fn run_with_options(
    conn: &Connection,
    db_path: &Path,
    machine_label: Option<String>,
    open_root_provider: OpenRootProvider,
) -> anyhow::Result<()> {
    run_with_initial_browse(conn, db_path, machine_label, None, open_root_provider).await
}

pub async fn run_with_initial_browse(
    conn: &Connection,
    db_path: &Path,
    machine_label: Option<String>,
    initial_browse: Option<InitialBrowse>,
    open_root_provider: OpenRootProvider,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(
        conn,
        db_path,
        &mut terminal,
        machine_label,
        initial_browse,
        open_root_provider,
    )
    .await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    match result {
        Ok(TuiExit::QuitNow) => std::process::exit(130),
        Ok(TuiExit::Normal) => Ok(()),
        Err(err) => Err(err),
    }
}

pub(super) enum TuiExit {
    Normal,
    QuitNow,
}

pub(super) async fn run_loop(
    conn: &Connection,
    db_path: &Path,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    _machine_label: Option<String>,
    initial_browse: Option<InitialBrowse>,
    open_root_provider: OpenRootProvider,
) -> anyhow::Result<TuiExit> {
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<TuiMessage>();
    let mut state = AppState {
        status: "loading database...".to_string(),
        temporary_browse: initial_browse.map(TemporaryBrowse::from),
        ..AppState::default()
    };
    let loading_selected_paths = BTreeSet::new();
    let loading_events = Vec::new();
    terminal.draw(|frame| {
        frame.render_widget(
            AppScreen {
                state: &state,
                roots: &[],
                files: &[],
                selected_paths: &loading_selected_paths,
                selected_root: None,
                selected_temporary: selected_temporary_browse(&state),
                summary: None,
                selection: None,
                detail_content: None,
                file_appearances: &[],
                events: &loading_events,
                root_count: visible_root_count(&state, 0),
                transfer_progress: None,
                import_progress: state.active_import_progress.as_ref(),
                detail_file_offset: 0,
            },
            frame.area(),
        );
    })?;
    state.status = "ready".to_string();
    loop {
        while let Ok(message) = job_rx.try_recv() {
            match message {
                TuiMessage::Status(message) => {
                    let level = if message.contains("failed") || message.contains(" error") {
                        ActivityLevel::Error
                    } else {
                        ActivityLevel::Info
                    };
                    state.background_finished(level, message);
                }
                TuiMessage::JobFinished { job_id, status } => {
                    let level = if status.contains("failed") || status.contains(" error") {
                        ActivityLevel::Error
                    } else {
                        ActivityLevel::Success
                    };
                    state.background_finished_job(&job_id, level, status);
                }
                TuiMessage::TransferFinished {
                    job_id,
                    plan_id,
                    copied: _copied,
                    skipped: _skipped,
                    errors,
                    canceled,
                    status,
                } => {
                    if state.transfer_run_plan_id.as_deref() == Some(plan_id.as_str()) {
                        state.transfer_run_plan_id = None;
                    }
                    refresh_last_plan(conn, &mut state, &plan_id)?;
                    let level = if status.contains("failed") {
                        ActivityLevel::Error
                    } else if canceled || errors > 0 {
                        ActivityLevel::Warning
                    } else {
                        ActivityLevel::Success
                    };
                    state.background_finished(level, status);
                    if errors > 0 {
                        append_transfer_error_activities(conn, &job_id, &mut state)?;
                    }
                    start_next_queued_transfer(conn, db_path, job_tx.clone(), &mut state)?;
                }
                TuiMessage::ImportFinished(status) => {
                    state.active_import_root_id = None;
                    state.active_import_progress = None;
                    let level = if status.contains("failed") {
                        ActivityLevel::Error
                    } else {
                        ActivityLevel::Success
                    };
                    state.background_finished(level, status);
                }
                TuiMessage::ImportProgress(progress) => {
                    state.active_import_root_id = Some(progress.root_id.clone());
                    state.active_import_progress = Some(progress.clone());
                    state.status = format!(
                        "importing {}: {} files indexed, {} queued",
                        progress.root_path, progress.files_imported, progress.files_queued
                    );
                }
                TuiMessage::OpenRootFinished(result) => match result {
                    Ok(result) => {
                        state.active_background_jobs =
                            state.active_background_jobs.saturating_sub(1);
                        if let Some(browse) = result.initial_browse {
                            state.temporary_browse = Some(TemporaryBrowse::from(browse));
                            state.selected_root = 0;
                            state.focus = FocusPane::Files;
                            state.file_offset = 0;
                        }
                        if let Some(root_id) = result.selected_root_id {
                            state.active_import_root_id = Some(root_id);
                            state.temporary_browse = None;
                            state.focus = FocusPane::Files;
                            state.file_offset = 0;
                        }
                        state.set_status(ActivityLevel::Success, result.status);
                    }
                    Err(err) => {
                        state.background_finished(
                            ActivityLevel::Error,
                            format!("open root failed: {err}"),
                        );
                    }
                },
                TuiMessage::TemporaryBrowseLoaded { path, result } => match result {
                    Ok(entries) => {
                        if let Some(browse) = state.temporary_browse.as_mut() {
                            browse.current_path = path.clone();
                            browse.entries = entries;
                        }
                        state.file_offset = 0;
                        state.background_finished(
                            ActivityLevel::Success,
                            format!("browsing {path}"),
                        );
                    }
                    Err(err) => {
                        state.background_finished(
                            ActivityLevel::Error,
                            format!("remote browse failed: {err}"),
                        );
                    }
                },
                TuiMessage::TemporaryTransferSourceImported {
                    root_id,
                    selected_relative_path,
                    mark_all,
                    status,
                } => {
                    mark_imported_transfer_source(
                        conn,
                        &root_id,
                        selected_relative_path.as_deref(),
                        mark_all,
                    )?;
                    if let Some(root) = db::root_by_id(conn, &root_id)? {
                        let selection = db::selection_summary_for_root(conn, &root_id)?;
                        state.transfer_plan_draft = Some(TransferPlanDraft {
                            source_root_id: root_id,
                            source_name: root_display_name(&root),
                            source_path: root.path,
                            marked_count: selection.marked_count,
                            marked_bytes: selection.marked_bytes,
                        });
                    }
                    state.focus = FocusPane::Roots;
                    state.background_finished(ActivityLevel::Success, status);
                }
                TuiMessage::FileAppearancesLoaded { key, result } => {
                    if state.active_file_appearance_key.as_deref() == Some(key.as_str()) {
                        state.active_file_appearance_key = None;
                    }
                    if state.file_appearance_key.as_deref() == Some(key.as_str()) {
                        match result {
                            Ok(appearances) => {
                                state.file_appearances = appearances;
                            }
                            Err(err) => {
                                state.file_appearances.clear();
                                state.set_status(
                                    ActivityLevel::Error,
                                    format!("file appearance lookup failed: {err}"),
                                );
                            }
                        }
                    }
                }
            }
        }
        let roots = db::roots(conn)?;
        select_active_import_root(&mut state, &roots);
        if state.active_background_jobs == 0 {
            state.active_import_root_id = None;
        }
        state.resumable_transfer_plans = resumable_transfer_plans(conn)?;
        let root_count = visible_root_count(&state, roots.len());
        normalize_selection(&mut state, root_count);
        let selected = selected_persisted_root(&roots, &state);
        let persisted_browse_dir = selected
            .map(|root| current_persisted_root_dir(&state, &root.id))
            .map(str::to_string);
        let (all_files, detail_key) = {
            let selected_temporary = selected_temporary_browse(&state);
            let files = match (selected, selected_temporary) {
                (Some(root), _) => db::cached_directory_entries(
                    conn,
                    &root.id,
                    persisted_browse_dir.as_deref().unwrap_or("."),
                )?
                .iter()
                .map(FileViewRow::from_cached_directory_entry)
                .collect(),
                (None, Some(browse)) => {
                    let indexed_entries = temporary_browse_indexed_entries(conn, &roots, browse)?;
                    let local_keys =
                        temporary_browse_local_availability_keys(conn, browse, &indexed_entries)?;
                    browse
                        .entries
                        .iter()
                        .map(|entry| {
                            let indexed = indexed_entries.get(entry.name.as_str());
                            FileViewRow::from_temporary_entry(
                                entry,
                                indexed,
                                if local_keys.contains(&entry.name) {
                                    1
                                } else {
                                    0
                                },
                            )
                        })
                        .collect()
                }
                (None, None) => Vec::new(),
            };
            (
                files,
                detail_selection_key(
                    selected,
                    selected_temporary,
                    persisted_browse_dir.as_deref(),
                ),
            )
        };
        let files = filtered_file_rows(&all_files, &state.file_filter);
        normalize_file_offset(&mut state, files.len());
        let event_root_id = state.last_plan.as_ref().and_then(|plan| {
            (state.focus == FocusPane::Plan).then_some(plan.source_root_id.as_str())
        });
        let events = match event_root_id.or_else(|| selected.map(|root| root.id.as_str())) {
            Some(root_id) => db::recent_jobs_and_events_for_root(conn, root_id, 300)?,
            None => db::recent_jobs_and_events(conn, 100)?,
        };
        let job_rows = job_rows(&events);
        if state.event_offset >= job_rows.len() {
            state.event_offset = job_rows.len().saturating_sub(1);
        }
        append_visible_transfer_error_activities(&events, &mut state);
        let summary = match selected {
            Some(root) => Some(db::root_summary(conn, &root.id)?),
            None => None,
        };
        let selection_summary = match selected {
            Some(root) => Some(db::selection_summary_for_root(conn, &root.id)?),
            None => None,
        };
        let selected_paths = match selected {
            Some(root) => db::selected_paths_for_root(conn, &root.id)?,
            None => BTreeSet::new(),
        };
        let transfer_progress = latest_transfer_progress(&events);
        state.sync_detail_selection(detail_key, files.len(), Instant::now());
        let detail_file = files.get(state.detail_file_offset);
        let detail_content = selected_file_content(conn, detail_file)?;
        sync_file_appearances(db_path, job_tx.clone(), &mut state, detail_file);
        let selected_temporary = selected_temporary_browse(&state);

        terminal.draw(|frame| {
            frame.render_widget(
                AppScreen {
                    state: &state,
                    roots: &roots,
                    files: &files,
                    selected_paths: &selected_paths,
                    selected_root: selected,
                    selected_temporary,
                    summary: summary.as_ref(),
                    selection: selection_summary.as_ref(),
                    detail_content: detail_content.as_ref(),
                    file_appearances: &state.file_appearances,
                    events: &job_rows,
                    root_count,
                    transfer_progress,
                    import_progress: state.active_import_progress.as_ref(),
                    detail_file_offset: state.detail_file_offset,
                },
                frame.area(),
            );
        })?;

        let poll_timeout = if state.active_background_jobs > 0 {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(5)
        };
        if event::poll(poll_timeout)? {
            if let Event::Key(key) = event::read()? {
                if is_interrupt_key(key) {
                    request_immediate_quit(conn, &mut state)?;
                    return Ok(TuiExit::QuitNow);
                }
                if state.file_filter_editing {
                    handle_file_filter_input(&mut state, key.code);
                    continue;
                }
                if state.retarget_draft.is_some() {
                    handle_retarget_input(conn, &mut state, key.code)?;
                    continue;
                }
                if state.pending_delete_root_id.is_some() {
                    handle_delete_root_confirmation(conn, &mut state, key.code)?;
                    continue;
                }
                if state.pending_import.is_some() {
                    handle_temporary_import_choice(&mut state, key.code, job_tx.clone());
                    continue;
                }
                if state.pending_open_root.is_some() {
                    handle_open_root_input(
                        &mut state,
                        key.code,
                        open_root_provider.clone(),
                        job_tx.clone(),
                    );
                    continue;
                }
                if state.pending_scoped_job.is_some() {
                    handle_scoped_job_choice(
                        conn,
                        db_path,
                        &roots,
                        &selected_paths,
                        key.code,
                        job_tx.clone(),
                        &mut state,
                    )?;
                    continue;
                }
                if state.transfer_plan_draft.is_some() {
                    handle_transfer_destination_modal_key(
                        conn,
                        &roots,
                        key.code,
                        terminal.size()?.height,
                        &mut state,
                    )?;
                    continue;
                }
                if state.focus == FocusPane::Plan
                    && (state.last_plan.is_some() || state.collection_result.is_some())
                {
                    handle_plan_modal_key(
                        conn,
                        db_path,
                        job_tx.clone(),
                        key.code,
                        terminal.size()?.height,
                        &mut state,
                    )?;
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => {
                        if state.active_background_jobs > 0 {
                            state.set_status(
                                ActivityLevel::Warning,
                                format!(
                                    "{} background job(s) still running; wait or cancel before quitting",
                                    state.active_background_jobs
                                ),
                            );
                            continue;
                        }
                        break;
                    }
                    KeyCode::Tab => state.focus = state.focus.next(),
                    KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        start_open_root_prompt(&mut state);
                    }
                    KeyCode::Char('/') if state.focus == FocusPane::Files => {
                        state.file_filter_editing = true;
                        state.status = if state.file_filter.is_empty() {
                            "file filter: type text, Enter keep, Esc clear".to_string()
                        } else {
                            format!("file filter: {}", state.file_filter)
                        };
                    }
                    KeyCode::Esc
                        if state.focus == FocusPane::Files && !state.file_filter.is_empty() =>
                    {
                        state.file_filter.clear();
                        state.file_offset = 0;
                        state.status = "file filter cleared".to_string();
                    }
                    KeyCode::Char('f') => {
                        state.file_view = state.file_view.next();
                        state.status = format!("file fields: {}", state.file_view.label());
                    }
                    KeyCode::Down => {
                        let plan_count = active_plan_row_count(&state);
                        move_down(
                            &mut state,
                            root_count,
                            files.len(),
                            plan_count,
                            job_rows.len(),
                        );
                    }
                    KeyCode::Up => move_up(&mut state),
                    KeyCode::Char('m') => {
                        verify_latest_collection_for_root(
                            conn,
                            selected_persisted_root(&roots, &state),
                            &mut state,
                        )?;
                    }
                    KeyCode::PageDown => {
                        let plan_count = active_plan_row_count(&state);
                        move_page_down(
                            &mut state,
                            root_count,
                            files.len(),
                            plan_count,
                            job_rows.len(),
                            visible_file_page_len(terminal.size()?.height),
                        );
                    }
                    KeyCode::PageUp => {
                        move_page_up(&mut state, visible_file_page_len(terminal.size()?.height));
                    }
                    KeyCode::Char('s') => {
                        queue_or_prompt_selected_root(
                            conn,
                            db_path,
                            selected_persisted_root(&roots, &state),
                            "scan",
                            &selected_paths,
                            job_tx.clone(),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('h') => {
                        queue_or_prompt_selected_root(
                            conn,
                            db_path,
                            selected_persisted_root(&roots, &state),
                            "hash",
                            &selected_paths,
                            job_tx.clone(),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('v') => {
                        queue_or_prompt_selected_root(
                            conn,
                            db_path,
                            selected_persisted_root(&roots, &state),
                            "verify",
                            &selected_paths,
                            job_tx.clone(),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('c') => {
                        request_selected_cancel(
                            conn,
                            job_rows.get(state.event_offset),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('t') => {
                        if selected_temporary_browse(&state).is_some() {
                            let file = files.get(state.file_offset).cloned();
                            start_temporary_transfer_source_import(
                                &mut state,
                                file.as_ref(),
                                job_tx.clone(),
                            );
                        } else {
                            start_transfer_plan_selection(
                                selected_persisted_root(&roots, &state),
                                selection_summary.as_ref(),
                                &mut state,
                            );
                        }
                    }
                    KeyCode::Char('p') => {
                        load_latest_transfer_plan(
                            conn,
                            selected_persisted_root(&roots, &state),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('x') => {
                        start_delete_root_confirmation(
                            selected_persisted_root(&roots, &state),
                            &mut state,
                        );
                    }
                    KeyCode::Char('i') => {
                        let file = files.get(state.file_offset);
                        start_temporary_import_prompt(&mut state, file);
                    }
                    KeyCode::Char('r') => {
                        if state.focus == FocusPane::Roots {
                            if let Some(plan_id) = selected_resume_plan(&state, roots.len())
                                .map(|plan| plan.id.clone())
                            {
                                load_transfer_plan_by_id(conn, &plan_id, &mut state)?;
                            }
                        }
                        run_current_transfer_plan(conn, db_path, job_tx.clone(), &mut state)?;
                    }
                    KeyCode::Char('a') => {
                        decide_current_plan_entry(
                            conn,
                            &mut state,
                            "copy",
                            "review accepted for copy",
                        )?;
                    }
                    KeyCode::Char('d') => {
                        decide_current_plan_entry(
                            conn,
                            &mut state,
                            "skip",
                            "review dropped by user",
                        )?;
                    }
                    KeyCode::Char('e') => {
                        start_retarget_current_plan_entry(&mut state);
                    }
                    KeyCode::Enter => {
                        if state.focus == FocusPane::Files
                            && selected_temporary_browse(&state).is_some()
                        {
                            let file = files.get(state.file_offset).cloned();
                            start_temporary_file_browse(&mut state, file.as_ref(), job_tx.clone());
                        } else if state.focus == FocusPane::Files {
                            let root_id = selected.map(|root| root.id.clone());
                            let file = files.get(state.file_offset).cloned();
                            open_persisted_file_entry(
                                &mut state,
                                root_id.as_deref(),
                                file.as_ref(),
                            );
                        } else if state.focus == FocusPane::Roots {
                            if let Some(plan_id) = selected_resume_plan(&state, roots.len())
                                .map(|plan| plan.id.clone())
                            {
                                load_transfer_plan_by_id(conn, &plan_id, &mut state)?;
                            } else {
                                create_transfer_plan_from_selection(conn, &roots, &mut state)?;
                            }
                        } else {
                            create_transfer_plan_from_selection(conn, &roots, &mut state)?;
                        }
                    }
                    KeyCode::Backspace => {
                        if state.focus == FocusPane::Files
                            && selected_temporary_browse(&state).is_some()
                        {
                            start_temporary_parent_browse(&mut state, job_tx.clone());
                        } else if state.focus == FocusPane::Files {
                            let root_id = selected.map(|root| root.id.clone());
                            open_persisted_parent(&mut state, root_id.as_deref());
                        }
                    }
                    KeyCode::Esc => {
                        if state.focus == FocusPane::Plan {
                            state.focus = FocusPane::Roots;
                            state.status = "plan closed".to_string();
                        } else {
                            cancel_transfer_plan_selection(&mut state);
                        }
                    }
                    KeyCode::Char(' ') => {
                        toggle_selected_file_mark(
                            conn,
                            selected,
                            files.get(state.file_offset),
                            &mut state,
                        )?;
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(TuiExit::Normal)
}

pub(super) fn handle_transfer_destination_modal_key(
    conn: &Connection,
    roots: &[db::RootRow],
    code: KeyCode,
    area_height: u16,
    state: &mut AppState,
) -> anyhow::Result<()> {
    match code {
        KeyCode::Esc => cancel_transfer_plan_selection(state),
        KeyCode::Enter => create_transfer_plan_from_selection(conn, roots, state)?,
        KeyCode::Down => move_down(state, visible_root_count(state, roots.len()), 0, 0, 0),
        KeyCode::Up => move_up(state),
        KeyCode::PageDown => {
            move_page_down(
                state,
                visible_root_count(state, roots.len()),
                0,
                0,
                0,
                visible_file_page_len(area_height),
            );
        }
        KeyCode::PageUp => move_page_up(state, visible_file_page_len(area_height)),
        _ => {
            state.status =
                "choose destination with arrows/PgUp/PgDn, Enter create plan, Esc cancel"
                    .to_string();
        }
    }
    Ok(())
}

pub(super) fn handle_plan_modal_key(
    conn: &Connection,
    db_path: &Path,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    code: KeyCode,
    area_height: u16,
    state: &mut AppState,
) -> anyhow::Result<()> {
    match code {
        KeyCode::Esc => {
            state.focus = FocusPane::Roots;
            state.status = "plan closed".to_string();
        }
        KeyCode::Down => move_down(state, 0, 0, active_plan_row_count(state), 0),
        KeyCode::Up => move_up(state),
        KeyCode::PageDown => {
            move_page_down(
                state,
                0,
                0,
                active_plan_row_count(state),
                0,
                visible_file_page_len(area_height),
            );
        }
        KeyCode::PageUp => move_page_up(state, visible_file_page_len(area_height)),
        KeyCode::Char('r') => run_current_transfer_plan(conn, db_path, job_tx, state)?,
        KeyCode::Char('a') => {
            decide_current_plan_entry(conn, state, "copy", "review accepted for copy")?;
        }
        KeyCode::Char('d') => {
            decide_current_plan_entry(conn, state, "skip", "review dropped by user")?;
        }
        KeyCode::Char('e') => start_retarget_current_plan_entry(state),
        KeyCode::Char('p') if state.collection_result.is_some() => {
            let root_id = state
                .collection_result
                .as_ref()
                .map(|collection| collection.root_id.clone());
            if let Some(root_id) = root_id {
                let root = db::root_by_id(conn, &root_id)?;
                load_latest_transfer_plan(conn, root.as_ref(), state)?;
            }
        }
        _ => {
            state.status =
                "plan modal: arrows/PgUp/PgDn move, r run, a accept, d drop, e retarget, Esc close"
                    .to_string();
        }
    }
    Ok(())
}

fn start_temporary_file_browse(
    state: &mut AppState,
    selected_file: Option<&FileViewRow>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    let Some(file) = selected_file else {
        state.status = "No remote entry selected".to_string();
        return;
    };
    if file.kind != FileKind::Directory {
        state.status = format!("selected remote file {}", file.relative_path);
        return;
    }
    let Some(current) = state
        .temporary_browse
        .as_ref()
        .map(|browse| browse.current_path.clone())
    else {
        state.status = "No temporary browse root selected".to_string();
        return;
    };
    let next_path = remote_child_path(&current, &file.relative_path);
    start_temporary_browse_load(state, next_path, job_tx);
}

fn start_temporary_parent_browse(state: &mut AppState, job_tx: mpsc::UnboundedSender<TuiMessage>) {
    let Some(browse) = state.temporary_browse.as_ref() else {
        state.status = "No temporary browse root selected".to_string();
        return;
    };
    if browse.current_path == browse.root_path {
        state.status = "Already at temporary root".to_string();
        return;
    }
    let Some(parent) = remote_parent_path(&browse.current_path, &browse.root_path) else {
        state.status = "Already at temporary root".to_string();
        return;
    };
    start_temporary_browse_load(state, parent, job_tx);
}

fn start_temporary_browse_load(
    state: &mut AppState,
    path: String,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    let Some(provider) = state
        .temporary_browse
        .as_ref()
        .and_then(|browse| browse.browse_provider.clone())
    else {
        state.status = "Remote browsing is unavailable for this temporary root".to_string();
        return;
    };
    state.background_started(format!("loading remote directory {path}"));
    task::spawn_blocking({
        let path = path.clone();
        move || {
            let result = provider(&path).map_err(|err| err.to_string());
            let _ = job_tx.send(TuiMessage::TemporaryBrowseLoaded { path, result });
        }
    });
}

fn start_open_root_prompt(state: &mut AppState) {
    state.pending_open_root = Some(OpenRootDraft::default());
    state.status = "open root: enter local path, file:// path, or host:/path".to_string();
}

fn handle_open_root_input(
    state: &mut AppState,
    key: KeyCode,
    provider: OpenRootProvider,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    let Some(draft) = state.pending_open_root.as_mut() else {
        return;
    };
    match key {
        KeyCode::Enter => {
            let target = draft.input.trim().to_string();
            state.pending_open_root = None;
            if target.is_empty() {
                state.status = "open root canceled: no location entered".to_string();
                return;
            }
            state.background_started(format!("opening root {target}"));
            task::spawn_blocking(move || {
                let result = provider(&target).map_err(|err| err.to_string());
                let _ = job_tx.send(TuiMessage::OpenRootFinished(result));
            });
        }
        KeyCode::Esc => {
            state.pending_open_root = None;
            state.status = "open root canceled".to_string();
        }
        KeyCode::Backspace => {
            draft.input.pop();
            state.status = if draft.input.is_empty() {
                "open root: enter local path, file:// path, or host:/path".to_string()
            } else {
                format!("open root: {}", draft.input)
            };
        }
        KeyCode::Char(ch) if !ch.is_control() => {
            draft.input.push(ch);
            state.status = format!("open root: {}", draft.input);
        }
        _ => {}
    }
}

fn selected_file_content(
    conn: &Connection,
    file: Option<&FileViewRow>,
) -> anyhow::Result<Option<db::ContentObjectRow>> {
    let Some(content_id) = file.and_then(|file| file.content_id.as_deref()) else {
        return Ok(None);
    };
    Ok(db::content_object_by_id(conn, content_id)?)
}

fn temporary_browse_local_availability_keys(
    conn: &Connection,
    browse: &TemporaryBrowse,
    indexed_entries: &BTreeMap<String, db::CachedDirectoryEntry>,
) -> rusqlite::Result<BTreeSet<String>> {
    let candidates = browse
        .entries
        .iter()
        .filter(|entry| entry.kind != "dir")
        .map(|entry| {
            let indexed = indexed_entries.get(&entry.name);
            db::LocalFileCandidate {
                key: &entry.name,
                content_id: indexed.and_then(|entry| entry.content_id.as_deref()),
                basename: &entry.name,
                size_bytes: entry.size_bytes,
                modified_at: entry.modified_at.as_deref(),
            }
        })
        .collect::<Vec<_>>();
    db::local_file_availability_keys(conn, &candidates)
}

fn temporary_browse_indexed_entries(
    conn: &Connection,
    roots: &[db::RootRow],
    browse: &TemporaryBrowse,
) -> anyhow::Result<BTreeMap<String, db::CachedDirectoryEntry>> {
    let Some((root, parent_path)) = roots
        .iter()
        .filter(|root| root.machine_id == browse.machine_id)
        .filter_map(|root| {
            remote_relative_parent_path(&root.path, &browse.current_path)
                .map(|parent| (root, parent))
        })
        .max_by_key(|(root, _)| remote_path_for_matching(&root.path).len())
    else {
        return Ok(BTreeMap::new());
    };
    Ok(db::cached_directory_entries(conn, &root.id, &parent_path)?
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect())
}

fn remote_relative_parent_path(root_path: &str, current_path: &str) -> Option<String> {
    let root_path = remote_path_for_matching(root_path);
    let current_path = current_path.trim_end_matches('/');
    if current_path == root_path {
        return Some(".".to_string());
    }
    current_path
        .strip_prefix(&format!("{root_path}/"))
        .map(|relative| {
            if relative.is_empty() {
                ".".to_string()
            } else {
                relative.to_string()
            }
        })
}

fn remote_path_for_matching(path: &str) -> &str {
    path.split_once(':')
        .map(|(_, remote)| remote)
        .unwrap_or(path)
        .trim_end_matches('/')
}

#[derive(Debug, Clone)]
struct FileAppearanceLookup {
    key: String,
    content_id: Option<String>,
    basename: String,
    size_bytes: u64,
    modified_at: Option<String>,
}

fn sync_file_appearances(
    db_path: &Path,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
    file: Option<&FileViewRow>,
) {
    let Some(lookup) = file_appearance_lookup(file) else {
        state.file_appearance_key = None;
        state.active_file_appearance_key = None;
        state.file_appearances.clear();
        return;
    };
    if state.file_appearance_key.as_deref() == Some(lookup.key.as_str()) {
        return;
    }

    state.file_appearance_key = Some(lookup.key.clone());
    state.active_file_appearance_key = Some(lookup.key.clone());
    state.file_appearances.clear();
    let db_path = db_path.to_path_buf();
    task::spawn_blocking(move || {
        let result = load_file_appearances(&db_path, &lookup);
        let _ = job_tx.send(TuiMessage::FileAppearancesLoaded {
            key: lookup.key,
            result,
        });
    });
}

fn load_file_appearances(
    db_path: &Path,
    lookup: &FileAppearanceLookup,
) -> Result<Vec<db::FileAppearanceRow>, String> {
    let conn = db::open_existing(db_path).map_err(|err| err.to_string())?;
    db::file_appearances(
        &conn,
        lookup.content_id.as_deref(),
        &lookup.basename,
        lookup.size_bytes,
        lookup.modified_at.as_deref(),
    )
    .map_err(|err| err.to_string())
}

fn file_appearance_lookup(file: Option<&FileViewRow>) -> Option<FileAppearanceLookup> {
    let file = file?;
    if file.kind != FileKind::File {
        return None;
    }
    let size_bytes = file.size_bytes.max(0) as u64;
    let basename = file_basename(&file.relative_path).to_string();
    let key = match file.content_id.as_deref() {
        Some(content_id) => format!("content:{content_id}"),
        None => format!(
            "stat:{}:{}:{}",
            basename,
            size_bytes,
            file.modified_at.as_deref().unwrap_or("-")
        ),
    };
    Some(FileAppearanceLookup {
        key,
        content_id: file.content_id.clone(),
        basename,
        size_bytes,
        modified_at: file.modified_at.clone(),
    })
}

fn file_basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(super) fn select_active_import_root(state: &mut AppState, roots: &[db::RootRow]) {
    let Some(root_id) = state.active_import_root_id.as_deref() else {
        return;
    };
    if let Some(idx) = roots.iter().position(|root| root.id == root_id) {
        state.selected_root = visible_index_for_persisted(state, idx);
    }
}

fn append_transfer_error_activities(
    conn: &Connection,
    job_id: &str,
    state: &mut AppState,
) -> anyhow::Result<()> {
    if job_id == "-" {
        return Ok(());
    }
    let events = db::events_for_job(conn, job_id)?
        .into_iter()
        .filter(|event| event.event_kind == "transfer_failed")
        .collect::<Vec<_>>();
    let visible = events.len().min(5);
    for event in events.iter().take(visible) {
        append_transfer_error_activity_once(
            state,
            transfer_error_activity_key(&event.job_id, event.sequence),
            &event.payload_json,
        );
    }
    if events.len() > visible {
        state.set_status(
            ActivityLevel::Error,
            format!(
                "{} more transfer error(s); inspect job {job_id}",
                events.len() - visible
            ),
        );
    }
    Ok(())
}

pub(super) fn append_visible_transfer_error_activities(
    events: &[db::JobEventRow],
    state: &mut AppState,
) {
    for event in events
        .iter()
        .filter(|event| event.event_kind == "transfer_failed")
    {
        append_transfer_error_activity_once(
            state,
            transfer_error_activity_key(&event.job_id, event.sequence),
            &event.payload_json,
        );
    }
    for event in events
        .iter()
        .filter(|event| event.job_kind == "transfer_copy" && event.errors > 0)
    {
        append_transfer_error_count_fallback(state, event);
    }
}

fn append_transfer_error_activity_once(state: &mut AppState, key: String, payload_json: &str) {
    if !state.transfer_error_activity_keys.insert(key) {
        return;
    }
    if let Some(message) = transfer_error_activity(payload_json) {
        state.set_status(ActivityLevel::Error, message);
    }
}

fn transfer_error_activity_key(job_id: &str, sequence: i64) -> String {
    format!("{job_id}:{sequence}")
}

fn append_transfer_error_count_fallback(state: &mut AppState, event: &db::JobEventRow) {
    let key_prefix = format!("{}:", event.job_id);
    if state
        .transfer_error_activity_keys
        .iter()
        .any(|key| key.starts_with(&key_prefix))
    {
        return;
    }
    let previous = state
        .transfer_error_count_by_job
        .get(&event.job_id)
        .copied()
        .unwrap_or(0);
    if event.errors <= previous {
        return;
    }
    state
        .transfer_error_count_by_job
        .insert(event.job_id.clone(), event.errors);
    let current_path = event.current_path.as_deref().unwrap_or("-");
    state.set_status(
        ActivityLevel::Error,
        format!(
            "transfer {} has {} error(s) but no failure reason is visible yet; latest path {}",
            short_id(&event.job_id),
            event.errors,
            truncate(current_path, 48)
        ),
    );
}

pub(super) fn transfer_error_activity(payload_json: &str) -> Option<String> {
    let payload: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    let relative_path = payload
        .get("relative_path")
        .and_then(|value| value.as_str())
        .unwrap_or("-");
    let error = payload
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or("transfer failed");
    Some(format!(
        "transfer error {}: {}",
        truncate(relative_path, 48),
        truncate(error, 140)
    ))
}

fn request_immediate_quit(conn: &Connection, state: &mut AppState) -> anyhow::Result<()> {
    let mut active_jobs = Vec::new();
    for job in db::active_jobs(conn)? {
        active_jobs.push(job);
    }
    let known_ids = active_jobs
        .iter()
        .map(|job| job.id.clone())
        .collect::<BTreeSet<_>>();
    for job_id in state.active_job_ids.difference(&known_ids) {
        if db::request_job_cancel(conn, job_id)? {
            append_interrupt_cancel_event(conn, job_id, "unknown", None, 0, 0)?;
        }
    }
    if let Some(plan_id) = state.transfer_run_plan_id.as_deref() {
        db::update_transfer_plan_status(conn, plan_id, "canceled")?;
    }
    let mut requested = 0_usize;
    for job in active_jobs {
        if db::request_job_cancel(conn, &job.id)? {
            requested += 1;
            append_interrupt_cancel_event(
                conn,
                &job.id,
                &job.kind,
                job.current_path.as_deref(),
                job.files_seen as u64,
                job.errors as u64,
            )?;
        }
        db::complete_job(conn, &job.id, "canceled")?;
        append_interrupt_canceled_event(
            conn,
            &job.id,
            &job.kind,
            job.current_path.as_deref(),
            job.files_seen as u64,
            job.errors as u64,
        )?;
        if job.kind == "transfer_copy" {
            if let Some(plan_id) = job
                .params_json
                .as_deref()
                .and_then(transfer_plan_id_from_job_params)
            {
                db::update_transfer_plan_status(conn, &plan_id, "canceled")?;
            }
        }
    }
    state.set_status(
        ActivityLevel::Warning,
        format!("interrupt requested; cancel marked for {requested} active job(s)"),
    );
    Ok(())
}

fn resumable_transfer_plans(conn: &Connection) -> anyhow::Result<Vec<db::TransferPlanRow>> {
    Ok(db::recent_transfer_plans(conn, 50)?
        .into_iter()
        .filter(|plan| matches!(plan.status.as_str(), "canceled" | "queued" | "running"))
        .collect())
}

fn append_interrupt_cancel_event(
    conn: &Connection,
    job_id: &str,
    kind: &str,
    path: Option<&str>,
    files_seen: u64,
    errors: u64,
) -> anyhow::Result<()> {
    let envelope = crate::events::EventEnvelope {
        event_kind: crate::events::EventKind::JobCancelRequested,
        job_id: Some(job_id.to_string()),
        sequence: None,
        created_at: crate::util::now_rfc3339(),
        payload: crate::events::EventPayload::Job {
            kind: kind.to_string(),
            path: path.map(str::to_string),
            message: Some("interrupt requested from tui".to_string()),
            files_seen: Some(files_seen),
            errors: Some(errors),
        },
    };
    db::persist_event(conn, &envelope)?;
    Ok(())
}

fn append_interrupt_canceled_event(
    conn: &Connection,
    job_id: &str,
    kind: &str,
    path: Option<&str>,
    files_seen: u64,
    errors: u64,
) -> anyhow::Result<()> {
    let envelope = crate::events::EventEnvelope {
        event_kind: crate::events::EventKind::JobCanceled,
        job_id: Some(job_id.to_string()),
        sequence: None,
        created_at: crate::util::now_rfc3339(),
        payload: crate::events::EventPayload::Job {
            kind: kind.to_string(),
            path: path.map(str::to_string),
            message: Some("interrupted by tui exit".to_string()),
            files_seen: Some(files_seen),
            errors: Some(errors),
        },
    };
    db::persist_event(conn, &envelope)?;
    Ok(())
}

fn transfer_plan_id_from_job_params(params_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(params_json)
        .ok()?
        .get("plan_id")?
        .as_str()
        .map(str::to_string)
}
