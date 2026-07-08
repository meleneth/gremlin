use super::*;
pub async fn run_with_options(
    conn: &Connection,
    db_path: &Path,
    machine_label: Option<String>,
) -> anyhow::Result<()> {
    run_with_initial_browse(conn, db_path, machine_label, None).await
}

pub async fn run_with_initial_browse(
    conn: &Connection,
    db_path: &Path,
    machine_label: Option<String>,
    initial_browse: Option<InitialBrowse>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(conn, db_path, &mut terminal, machine_label, initial_browse).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

pub(super) async fn run_loop(
    conn: &Connection,
    db_path: &Path,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    machine_label: Option<String>,
    initial_browse: Option<InitialBrowse>,
) -> anyhow::Result<()> {
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<TuiMessage>();
    let mut state = AppState {
        status: "ready".to_string(),
        temporary_browse: initial_browse.map(TemporaryBrowse::from),
        ..AppState::default()
    };
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
                TuiMessage::TransferFinished { plan_id, status } => {
                    if state.transfer_run_plan_id.as_deref() == Some(plan_id.as_str()) {
                        state.transfer_run_plan_id = None;
                    }
                    refresh_last_plan(conn, &mut state, &plan_id)?;
                    let level = if status.contains("failed") {
                        ActivityLevel::Error
                    } else if status.contains("canceled") || !status.contains("errors 0") {
                        ActivityLevel::Warning
                    } else {
                        ActivityLevel::Success
                    };
                    state.background_finished(level, status);
                }
                TuiMessage::ImportFinished(status) => {
                    let level = if status.contains("failed") {
                        ActivityLevel::Error
                    } else {
                        ActivityLevel::Success
                    };
                    state.background_finished(level, status);
                }
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
                    state.transfer_source_root_id = Some(root_id);
                    state.focus = FocusPane::Roots;
                    state.background_finished(ActivityLevel::Success, status);
                }
            }
        }
        let roots = db::roots(conn)?;
        let root_count = visible_root_count(&state, roots.len());
        normalize_selection(&mut state, root_count);
        let selected = selected_persisted_root(&roots, &state);
        let selected_temporary = selected_temporary_browse(&state);
        let files = match (selected, selected_temporary) {
            (Some(root), _) => db::cached_directory_entries(
                conn,
                &root.id,
                current_persisted_root_dir(&state, &root.id),
            )?
            .iter()
            .map(FileViewRow::from_cached_directory_entry)
            .collect(),
            (None, Some(browse)) => browse
                .entries
                .iter()
                .map(FileViewRow::from_temporary_entry)
                .collect(),
            (None, None) => Vec::new(),
        };
        let event_root_id = state.last_plan.as_ref().and_then(|plan| {
            (state.focus == FocusPane::Plan).then_some(plan.source_root_id.as_str())
        });
        let events = match event_root_id.or_else(|| selected.map(|root| root.id.as_str())) {
            Some(root_id) => db::recent_jobs_and_events_for_root(conn, root_id, 300)?,
            None => db::recent_jobs_and_events(conn, 100)?,
        };
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
                    events: &events,
                    root_count,
                    transfer_progress,
                },
                frame.area(),
            );
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
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
                    KeyCode::Char('v') => {
                        state.file_view = state.file_view.next();
                        state.status = format!("file fields: {}", state.file_view.label());
                    }
                    KeyCode::Down => {
                        let plan_count = state
                            .last_plan
                            .as_ref()
                            .map(|plan| plan.entries.len())
                            .unwrap_or(0);
                        move_down(
                            &mut state,
                            root_count,
                            files.len(),
                            plan_count,
                            events.len(),
                        );
                    }
                    KeyCode::Up => move_up(&mut state),
                    KeyCode::Char('s') => {
                        queue_selected_root(
                            conn,
                            db_path,
                            selected_persisted_root(&roots, &state),
                            "scan",
                            machine_label.as_deref(),
                            job_tx.clone(),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('h') => {
                        queue_selected_root(
                            conn,
                            db_path,
                            selected_persisted_root(&roots, &state),
                            "hash",
                            machine_label.as_deref(),
                            job_tx.clone(),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('V') => {
                        queue_selected_root(
                            conn,
                            db_path,
                            selected_persisted_root(&roots, &state),
                            "verify",
                            machine_label.as_deref(),
                            job_tx.clone(),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('c') => {
                        request_selected_cancel(conn, events.get(state.event_offset), &mut state)?;
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
                        run_current_transfer_plan(db_path, job_tx.clone(), &mut state);
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
                            open_temporary_file_entry(&mut state, file.as_ref());
                        } else if state.focus == FocusPane::Files {
                            let root_id = selected.map(|root| root.id.clone());
                            let file = files.get(state.file_offset).cloned();
                            open_persisted_file_entry(
                                &mut state,
                                root_id.as_deref(),
                                file.as_ref(),
                            );
                        } else {
                            create_transfer_plan_from_selection(conn, &roots, &mut state)?;
                        }
                    }
                    KeyCode::Backspace => {
                        if state.focus == FocusPane::Files
                            && selected_temporary_browse(&state).is_some()
                        {
                            open_temporary_parent(&mut state);
                        } else if state.focus == FocusPane::Files {
                            let root_id = selected.map(|root| root.id.clone());
                            open_persisted_parent(&mut state, root_id.as_deref());
                        }
                    }
                    KeyCode::Esc => {
                        cancel_transfer_plan_selection(&mut state);
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
    Ok(())
}
